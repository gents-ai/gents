use crate::support::graphql::{graphql_mutation_with_variables, served_endpoint};
use crate::support::mocks::fake_llm::{ChatAction, FakeLlm, Responder};
use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents::tools::background::job::keys::{job_session_id, job_spawn_key};
use gents_protocol::job::{
    JobBudget, JobCeiling, JobEffects, JobGuestPin, JobOrigin, JobPauseReason, JobPin, JobProgress,
    JobSpawn, JobState, JobStepOutput,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A home served by `gents server`, with the identity its job rows belong to.
pub(crate) struct ServedHome {
    _tempdir: tempfile::TempDir,
    pub(crate) home: PathBuf,
    pub(crate) graphql: String,
    pub(crate) node_did: String,
    pub(crate) agent_id: String,
    _llm: FakeLlm,
    _serve: ServeProcess,
}

impl ServedHome {
    pub(crate) async fn start() -> Result<Self> {
        Self::start_with(Arc::new(|_: &Value| ChatAction::Hang), false).await
    }

    /// Serves a node whose provider is the scripted `responder`. `job_fixture`
    /// makes bash read-write and installs and wires the job fixture pack with
    /// the trusted-pack job block before the server starts.
    pub(crate) async fn start_with(responder: Responder, job_fixture: bool) -> Result<Self> {
        let tempdir = tempfile::tempdir().context("creating tempdir")?;
        let home = tempdir.path().join("home");
        fs::create_dir_all(&home)?;
        let model_name = format!("mock-job-cli-{}", Uuid::new_v4().simple());
        let llm = FakeLlm::start(&model_name, None, responder)?;
        let port = allocate_port()?;
        let graphql = graphql_url(port);
        let node_name = format!("cli-job-{}", Uuid::new_v4().simple());
        let mut init_args = vec![
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            llm.endpoint(),
        ];
        if job_fixture {
            // A job child held by the provider must not take the only slot
            // other sessions need while the job runs.
            init_args.extend(["--write", "--max-concurrent", "4"]);
        }
        let init = run_init_json(&home, &init_args)?;
        let node_did = node_did_from_init(&init)?;
        let agent_id = init["default_agent_id"]
            .as_str()
            .context("init output missing default_agent_id")?
            .to_owned();
        if job_fixture {
            let pack = tempdir.path().join("job_plugin_fixture");
            copy_dir_all(&fixture_pack_dir("job_plugin_fixture"), &pack)?;
            write_json_file(
                &pack.join("pack_config.json"),
                &json!({
                    "node": {},
                    "agent_targets": [{
                        "target_id": agent_id,
                        "target_node_did": node_did,
                        "agent_id": agent_id,
                        "name": "job child",
                    }],
                    "tools": [{
                        "tools_id": "job-fixture-caller",
                        "display_name": "Job fixture caller",
                        "integrations": {"plugins": [{
                            "plugin": "fixture/job_fixture",
                            "job": {
                                "agent_targets": [agent_id],
                                "policy": {"trusted_packs": ["fixture/job_plugin_fixture"]},
                            },
                        }]},
                    }],
                }),
            )?;
            let pack_arg = pack.to_str().context("pack path is not UTF-8")?;
            run_cli_json(
                &home,
                &[
                    "pack",
                    "install",
                    pack_arg,
                    "--registry",
                    "http://127.0.0.1:9",
                ],
            )?;
            run_cli_json(
                &home,
                &["pack", "wire", "fixture/job_fixture", "--agent", &agent_id],
            )?;
        }
        let mut serve = spawn_server(&home, port)?;
        wait_for_port(port, &mut serve)?;
        wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
        Ok(Self {
            _tempdir: tempdir,
            home,
            graphql,
            node_did,
            agent_id,
            _llm: llm,
            _serve: serve,
        })
    }

    pub(crate) fn access(&self) -> ConfigAccess {
        ConfigAccess::Graphql(served_endpoint(&self.graphql))
    }

    /// `gents job <args>` against this node: (exit code, stdout, stderr).
    pub(crate) fn job(&self, args: &[&str]) -> Result<(Option<i32>, String, String)> {
        let mut full = vec!["job"];
        full.extend_from_slice(args);
        full.extend(["--graphql", &self.graphql]);
        run_job(&self.home, &full)
    }

    /// `gents goal <args>` against this node.
    pub(crate) fn goal(&self, args: &[&str]) -> Result<Value> {
        let mut full = vec!["goal"];
        full.extend_from_slice(args);
        full.extend(["--graphql", &self.graphql]);
        run_cli_json(&self.home, &full)
    }
}

fn run_job(cwd: &Path, args: &[&str]) -> Result<(Option<i32>, String, String)> {
    let output = Command::new(cli_bin())
        .env("HOME", cwd)
        .env("RUST_LOG", "error")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))?;
    Ok((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Seeds one plugin job row and returns its doc id. `lifecycle` and `state`
/// are the row's persisted values (`job_state` reads both).
#[expect(
    clippy::too_many_arguments,
    reason = "test seed helper takes the seeded row's fields"
)]
pub(crate) async fn seed_job(
    access: &ConfigAccess,
    node_did: &str,
    request_doc_id: &str,
    session: &str,
    call: &str,
    lifecycle: &str,
    state: JobState,
    pause_reason: Option<JobPauseReason>,
) -> Result<String> {
    let key = format!("{session}:{call}");
    let mut pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: call.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let progress = JobProgress {
        title: format!("Job {call}"),
        state,
        pause_reason,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let completed_at = if lifecycle == "running" {
        r#"deadline_at: "2126-06-01T10:00:00Z","#.to_owned()
    } else {
        r#"completed_at: "2026-06-01T10:05:00Z","#.to_owned()
    };
    let mutation = format!(
        r#"mutation($job_pin: JSON, $job_progress: JSON) {{ create_AgentToolCall(input: {{
            tool_call_key: "{key}", request_id: "request-{call}", request_doc_id: "{request_doc_id}",
            session_id: "{session}", node_did: "{node_did}", requester_did: "{node_did}", tool_name: "workflow",
            tool_call_id: "{call}", message_sequence: 1, await_mode: "background", status: "{lifecycle}", lifecycle_state: "{lifecycle}",
            started_at: "2026-06-01T10:00:00Z", {completed_at}
            job_pin: $job_pin, job_progress: $job_progress
        }}) {{ _docID }} }}"#,
        key = escape_graphql_string(&key),
        request_doc_id = escape_graphql_string(request_doc_id),
        session = escape_graphql_string(session),
        node_did = escape_graphql_string(node_did),
        call = escape_graphql_string(call),
        lifecycle = escape_graphql_string(lifecycle),
    );
    let response = graphql_mutation_with_variables(
        access,
        &mutation,
        &json!({ "job_pin": pin, "job_progress": progress }),
    )
    .await?;
    let row_doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentToolCall")?;
    // The first row of a chain is its own root, known only once it exists.
    pin.chain_root_doc_id = row_doc_id.clone();
    graphql_mutation_with_variables(
        access,
        &format!(
            r#"mutation($job_pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $job_pin }}) {{ _docID }} }}"#,
            escape_graphql_string(&row_doc_id)
        ),
        &json!({ "job_pin": pin }),
    )
    .await?;
    Ok(row_doc_id)
}

/// Seeds a terminal child request the job at `row_doc_id` spawned in `slot`.
pub(crate) async fn seed_child(
    access: &ConfigAccess,
    node_did: &str,
    row_doc_id: &str,
    slot: &str,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "{request}", purpose: "normal", node_did: "{node_did}",
            agent_id: "default", session_id: "{session}", retry_key: "{retry}",
            content: "child {slot}", lifecycle_state: "failed", failure_reason: "seeded",
            caused_by_parent_tool_call_doc_id: "{row}",
            created_at: "2026-06-01T10:01:00Z", terminalized_at: "2026-06-01T10:02:00Z"
        }}) {{ _docID }} }}"#,
        request = escape_graphql_string(&format!("child-{row_doc_id}-{slot}")),
        node_did = escape_graphql_string(node_did),
        session = escape_graphql_string(&job_session_id(row_doc_id, slot, 0)),
        retry = escape_graphql_string(&job_spawn_key(row_doc_id, slot, 0)),
        row = escape_graphql_string(row_doc_id),
    );
    graphql_mutation_with_variables(access, &mutation, &json!({})).await?;
    Ok(())
}

/// Publishes the accepted assistant message that admits the tool row `row_doc_id`
/// at sequence 1, which a Local stop loads before it ends the row.
pub(crate) async fn seed_admission(
    access: &ConfigAccess,
    node_did: &str,
    request_doc_id: &str,
    session: &str,
    row_doc_id: &str,
    call: &str,
) -> Result<()> {
    let arguments = "{}";
    let generation = format!("cli-job-fixture:{request_doc_id}");
    let created_at = "2026-06-01T10:00:00Z";
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: Some(node_did.into()),
        session_id: session.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: arguments.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: call.into(),
                    call_id: Some(call.into()),
                    name: "workflow".into(),
                },
            }),
        }],
        payload: arguments.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![arguments.len() as u64],
        }),
        created_at: created_at.into(),
    };
    let response = graphql_mutation_with_variables(
        access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentOutputSegment")?;
    let message = TranscriptMessage {
        message_key: gents::session::sequence_message_key(node_did, session, Some(node_did), 1),
        session_id: session.into(),
        node_did: node_did.into(),
        requester_did: Some(node_did.into()),
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence: 1,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: row_doc_id.into(),
            id: call.into(),
            call_id: Some(call.into()),
            name: "workflow".into(),
            arguments: PayloadRef {
                close_doc_id,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: created_at.into(),
    };
    graphql_mutation_with_variables(
        access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&message)?,
    )
    .await?;
    Ok(())
}

/// Publishes the background receipt a detached job row needs before its terminal.
pub(crate) async fn seed_receipt(
    access: &ConfigAccess,
    node_did: &str,
    request_doc_id: &str,
    session: &str,
    row_doc_id: &str,
    call: &str,
) -> Result<()> {
    let text = "started";
    let created_at = "2026-06-01T10:00:01Z";
    let key = format!("{session}:background-receipt:{row_doc_id}");
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: Some(node_did.into()),
        session_id: session.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored { key: key.clone() },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: row_doc_id.into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: created_at.into(),
    };
    let response = graphql_mutation_with_variables(
        access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentOutputSegment")?;
    let message = TranscriptMessage {
        message_key: key,
        session_id: session.into(),
        node_did: node_did.into(),
        requester_did: Some(node_did.into()),
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: row_doc_id.into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 2,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::ToolResult {
            tool_call_doc_id: row_doc_id.into(),
            id: call.into(),
            call_id: Some(call.into()),
            parts: vec![ToolResultPart::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
        }],
        created_at: created_at.into(),
    };
    graphql_mutation_with_variables(
        access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&message)?,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn served_node_lists_shows_and_watches_ended_jobs() -> Result<()> {
    let served = ServedHome::start().await?;
    let (access, did) = (served.access(), served.node_did.as_str());
    let done = seed_job(
        &access,
        did,
        "",
        "job-s1",
        "call-done",
        "completed",
        JobState::Completed,
        None,
    )
    .await?;
    for slot in ["alpha", "beta"] {
        seed_child(&access, did, &done, slot).await?;
    }
    seed_job(
        &access,
        did,
        "",
        "job:s2",
        "call-paused",
        "completed",
        JobState::Paused,
        Some(JobPauseReason::User),
    )
    .await?;
    seed_job(
        &access,
        did,
        "",
        "job-s1",
        "call-cancelled",
        "cancelled",
        JobState::Cancelled,
        None,
    )
    .await?;

    let (code, stdout, stderr) = served.job(&["list", "--state", "all", "--output", "json"])?;
    assert_eq!(code, Some(0), "{stderr}");
    let listed: Value = serde_json::from_str(&stdout)?;
    let mut addresses: Vec<&str> = listed
        .as_array()
        .context("list prints an array")?
        .iter()
        .filter_map(|view| view["address"].as_str())
        .collect();
    addresses.sort_unstable();
    assert_eq!(
        addresses,
        [
            "job-s1:call-cancelled",
            "job-s1:call-done",
            "job:s2:call-paused"
        ]
    );

    let (code, stdout, stderr) = served.job(&["show", "job-s1:call-done"])?;
    assert_eq!(code, Some(0), "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("completed"), "answer first: {stdout}");
    assert!(
        stdout.contains("children:") && stdout.contains("alpha") && stdout.contains("beta"),
        "{stdout}"
    );
    assert!(
        stdout.contains("gents job show job-s1:call-done"),
        "{stdout}"
    );

    for (address, expected_code, state) in [
        ("job-s1:call-done", 0, "completed"),
        ("job:s2:call-paused", 3, "paused"),
    ] {
        let (code, stdout, stderr) =
            served.job(&["watch", address, "--until-terminal", "--output", "json"])?;
        assert_eq!(code, Some(expected_code), "{address}: {stderr}");
        assert_eq!(stdout.lines().count(), 1, "{stdout}");
        let line: Value = serde_json::from_str(stdout.trim())?;
        assert_eq!(line["address"], address);
        assert_eq!(line["state"], state);
    }

    let (code, _, stderr) = served.job(&["stop", "job-s1:call-done"])?;
    assert_ne!(code, Some(0));
    assert!(stderr.contains("job control is local-only"), "{stderr}");
    let (_, stdout, _) = served.job(&["show", "job-s1:call-done"])?;
    assert!(
        stdout.starts_with("completed"),
        "stop changed the row: {stdout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_access_refuses_watch_and_stops_a_running_job_on_an_offline_home() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let node_home = tempdir.path().join("node-home");
    let init = run_init_json(
        tempdir.path(),
        &[
            "--identity-only",
            "--node-name",
            "cli-job-offline",
            "--home",
            node_home.to_str().context("node home utf8")?,
        ],
    )?;
    let did = node_did_from_init(&init)?;
    {
        let node = initialized_node(tempdir.path(), &node_home, "cli-job-offline").await?;
        gents::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local(Arc::new(node));
        let create = gents::build_signed_request(
            gents::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents::RequestIdentity {
                    request_id: "request-call-running".into(),
                    node_did: did.clone(),
                    requester_did: None,
                    agent_id: "general".into(),
                    session_id: "job-s1".into(),
                    content: "run".into(),
                    execution_origin: gents::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: "2026-06-01T10:00:00Z".into(),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
            ),
            gents::RequestSigner::RegisteredTarget,
        )
        .await?;
        let mutation = create
            .graphql_mutation()
            .map_err(|error| anyhow::anyhow!(error))?;
        let request = graphql_mutation_with_variables(&access, &mutation, &json!({})).await?;
        let request_doc_id =
            gents_protocol::graphql::extract_mutation_doc_id(&request, "AgentRequest")?;
        // A claimed request is processing; claim is not part of the signed receipt.
        graphql_mutation_with_variables(
            &access,
            &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "processing" }}) {{ _docID }} }}"#,
                escape_graphql_string(&request_doc_id)
            ),
            &json!({}),
        )
        .await?;
        let row = seed_job(
            &access,
            &did,
            &request_doc_id,
            "job-s1",
            "call-running",
            "running",
            JobState::Running,
            None,
        )
        .await?;
        seed_admission(
            &access,
            &did,
            &request_doc_id,
            "job-s1",
            &row,
            "call-running",
        )
        .await?;
        seed_receipt(
            &access,
            &did,
            &request_doc_id,
            "job-s1",
            &row,
            "call-running",
        )
        .await?;
    }
    let home = node_home.to_str().context("node home utf8")?;

    let (code, _, stderr) = run_job(
        tempdir.path(),
        &[
            "job",
            "watch",
            "job-s1:call-running",
            "--until-terminal",
            "--home",
            home,
        ],
    )?;
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("no running server for this home"),
        "{stderr}"
    );

    let (code, stdout, stderr) = run_job(
        tempdir.path(),
        &["job", "stop", "job-s1:call-running", "--home", home],
    )?;
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.starts_with("cancelled  stop applied"), "{stdout}");
    Ok(())
}

fn message_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(content) => content.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn last_user_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
        .map(message_text)
        .unwrap_or_default()
}

fn tool_result_texts(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .map(message_text)
        .collect()
}

fn calls_tool(request: &Value, name: &str) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .any(|call| call["function"]["name"] == name)
}

async fn children_of(graphql: &str, request_id: &str) -> Result<Vec<Value>> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
        AgentRequest(filter: {{ {} }}) {{
            _docID request_id input caused_by_trigger_kind caused_by_parent_request_id
        }}
    }}"#,
            gents::session::public_request_filter(&format!(
                r#"caused_by_parent_request_id: {{ _eq: "{}" }}"#,
                escape_graphql_string(request_id)
            ))
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("children of the held request")?
        .clone())
}

/// A Goal-held Task does not continue while a process held on a job runs: the
/// background-completion delivery, not a goal child, carries it on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_held_task_defers_while_job_runs() -> Result<()> {
    const LAUNCH: &str = "launch the fixture job";
    const CHILD: &str = "goal job child a";
    const PREDECESSOR: &str = "ready the goal session";
    const OBJECTIVE: &str = "watch the fixture job until it ends";
    let s_job = format!("goal-job-session-{}", Uuid::new_v4().simple());
    let s_goal = format!("goal-held-session-{}", Uuid::new_v4().simple());

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let released = Arc::new(AtomicBool::new(false));
    let watch_address = Arc::new(Mutex::new(String::new()));
    // Tool commands cannot read the runtime home's credentials, so a command
    // cannot watch the job as the node; the test releases this pipe once the
    // job row is terminal.
    let job_ended = Arc::new(Mutex::new(String::new()));
    let spawn_handle = Arc::new(Mutex::new(String::new()));
    // The responder is built before the node starts: (gents home, graphql, agent id).
    let node = Arc::new(Mutex::new(None::<(String, String, String)>));
    let asset = fs::read(fixture_pack_dir("job_plugin_fixture").join("fixture_assets/a.txt"))?;
    let digest = format!("sha256:{:x}", Sha256::digest(&asset));

    let responder: Responder = {
        let gate = gate.clone();
        let released = released.clone();
        let job_ended = job_ended.clone();
        let spawn_handle = spawn_handle.clone();
        let node = node.clone();
        Arc::new(move |request: &Value| {
            let user = last_user_text(request);
            let results = tool_result_texts(request);
            let node_state = node.lock().unwrap().clone();
            if user.contains(CHILD) {
                return ChatAction::WaitThenSse(gate.clone(), completion_text_sse("child a done"));
            }
            if user.contains(LAUNCH) {
                if !results.is_empty() {
                    return ChatAction::Sse(completion_text_sse("job launched"));
                }
                let (_, _, agent_id) = node_state.expect("node state is set before submits");
                let e0 = JobStepOutput::Effects {
                    effects: JobEffects {
                        state: None,
                        pin: Some(JobGuestPin {
                            script_id: "fixture".into(),
                            source_digest: digest.clone(),
                            agent_budget: 8,
                            handle: "goal-job".into(),
                        }),
                        resume: None,
                        spawn: vec![JobSpawn {
                            slot: "a".into(),
                            attempt: 0,
                            target: agent_id,
                            prompt: CHILD.into(),
                            title: "a".into(),
                            cwd: None,
                        }],
                        message: Vec::new(),
                        interrupt: Vec::new(),
                        progress: None,
                        writes: Vec::new(),
                        scratch: Vec::new(),
                    },
                };
                let e1 = JobStepOutput::Result {
                    result: json!("job done"),
                    writes: Vec::new(),
                    scratch: Vec::new(),
                    progress: None,
                    control: Vec::new(),
                    pin: None,
                    resume: None,
                };
                let holding = JobStepOutput::Effects {
                    effects: JobEffects {
                        state: None,
                        pin: None,
                        resume: None,
                        spawn: Vec::new(),
                        message: Vec::new(),
                        interrupt: Vec::new(),
                        progress: None,
                        writes: Vec::new(),
                        scratch: Vec::new(),
                    },
                };
                let plan = json!([
                    {"when": {"step": 0}, "output": serde_json::to_value(&e0).unwrap()},
                    {"when": {"children": {"a": "completed"}}, "output": serde_json::to_value(&e1).unwrap()},
                    {"when": {"children": {"a": "live"}}, "output": serde_json::to_value(&holding).unwrap()},
                ]);
                return ChatAction::Sse(tool_call_sse_with_id(
                    "job-call-1",
                    "job_fixture",
                    &json!({"args": {"plan": plan}}).to_string(),
                ));
            }
            if user.contains(PREDECESSOR) {
                return ChatAction::Sse(completion_text_sse("goal session ready"));
            }
            let held_turn = request["messages"].to_string().contains(OBJECTIVE);
            if held_turn && !released.load(Ordering::SeqCst) {
                return match results.len() {
                    0 => ChatAction::Sse(tool_call_sse_with_id(
                        "watch-spawn-1",
                        "spawn_process",
                        &json!({"tool_name": "bash_unrestricted", "args": {
                            "command": "cat",
                            "args": [job_ended.lock().unwrap().as_str()],
                        }})
                        .to_string(),
                    )),
                    1 => {
                        let reply: Value = serde_json::from_str(&results[0]).unwrap_or_default();
                        let handle = reply["tool_call_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        *spawn_handle.lock().unwrap() = handle.clone();
                        ChatAction::Sse(tool_call_sse_with_id(
                            "watch-wait-1",
                            "wait_process",
                            &json!({"tool_call_id": handle, "timeout_secs": 2}).to_string(),
                        ))
                    }
                    _ => ChatAction::Sse(completion_text_sse("waiting on the job")),
                };
            }
            let watched = spawn_handle.lock().unwrap().clone();
            let completed_watch = !watched.is_empty()
                && request["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|message| message["role"] == "user")
                    .map(message_text)
                    .any(|text| {
                        text.contains("<tool-completion ")
                            && text.contains(&format!("tool_call_id=\"{watched}\""))
                            && text.contains("status=\"completed\"")
                    });
            if released.load(Ordering::SeqCst) && completed_watch {
                return if calls_tool(request, "update_goal") {
                    ChatAction::Sse(completion_text_sse("goal complete"))
                } else {
                    ChatAction::Sse(tool_call_sse_with_id(
                        "goal-done-1",
                        "update_goal",
                        r#"{"status":"complete"}"#,
                    ))
                };
            }
            ChatAction::Sse(completion_text_sse("session title"))
        })
    };

    let served = ServedHome::start_with(responder, true).await?;
    let g = served.graphql.clone();
    let h = served
        .home
        .join(".gents")
        .to_str()
        .context("gents home is not UTF-8")?
        .to_owned();
    *node.lock().unwrap() = Some((h, g.clone(), served.agent_id.clone()));

    // Launch the job in its own session; it is a background row once the launching request ends.
    run_cli_json(
        &served.home,
        &[
            "request",
            "submit",
            "--graphql",
            &g,
            "--node-did",
            &served.node_did,
            "--session-id",
            &s_job,
            "--content",
            LAUNCH,
            "--timeout-secs",
            "60",
            "--poll-secs",
            "1",
        ],
    )?;
    let job_rows = graphql_query(
        &g,
        &format!(
            "{{ AgentToolCall(filter: {{ session_id: {{ _eq: \"{}\" }}, tool_name: {{ _eq: \"job_fixture\" }} }}) {{ tool_call_id }} }}",
            escape_graphql_string(&s_job)
        ),
    )
    .await?;
    let job_call_id = job_rows
        .pointer("/data/AgentToolCall/0/tool_call_id")
        .and_then(Value::as_str)
        .with_context(|| format!("job row: {job_rows}"))?;
    *watch_address.lock().unwrap() = format!("{s_job}:{job_call_id}");
    let job_ended_pipe = served.home.join("job-ended");
    let made = Command::new("mkfifo").arg(&job_ended_pipe).status()?;
    anyhow::ensure!(
        made.success(),
        "mkfifo {}: {made}",
        job_ended_pipe.display()
    );
    *job_ended.lock().unwrap() = job_ended_pipe
        .to_str()
        .context("job-ended pipe path is not UTF-8")?
        .to_owned();

    // Hold the Goal on the job.
    let predecessor_receipt = run_cli_json(
        &served.home,
        &[
            "request",
            "submit",
            "--graphql",
            &g,
            "--node-did",
            &served.node_did,
            "--session-id",
            &s_goal,
            "--content",
            PREDECESSOR,
            "--timeout-secs",
            "60",
            "--poll-secs",
            "1",
        ],
    )?;
    let predecessor = predecessor_receipt["request_id"]
        .as_str()
        .context("predecessor request id")?
        .to_owned();
    served.goal(&[
        "set",
        "--session",
        &s_goal,
        "--objective",
        OBJECTIVE,
        "--status",
        "paused",
        "--token-budget",
        "200000",
    ])?;
    let resumed = served.goal(&[
        "resume-request",
        "--session",
        &s_goal,
        "--from",
        &predecessor,
    ])?;
    assert_eq!(resumed["created"], true, "{resumed}");
    let held = resumed["request_id"]
        .as_str()
        .context("held request id")?
        .to_owned();
    let opening_doc = resumed["doc_id"]
        .as_str()
        .context("Goal opening request document")?;
    let binding = graphql_query(
        &g,
        &format!(
            "{{ Goal(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{goal_id assignment_root_request_doc_id}} }}",
            escape_graphql_string(&served.node_did),
            escape_graphql_string(&s_goal),
        ),
    )
    .await?;
    assert_eq!(
        binding["data"]["Goal"][0]["assignment_root_request_doc_id"],
        opening_doc
    );
    let children = children_of(&g, &predecessor).await?;
    assert_eq!(children.len(), 1, "{children:?}");
    assert_eq!(children[0]["_docID"], opening_doc);
    assert_eq!(
        children[0]["input"]["goal_assignment"],
        json!({"goal_id": resumed["goal_id"], "root_request_doc_id": null}),
    );
    let replay = served.goal(&[
        "resume-request",
        "--session",
        &s_goal,
        "--from",
        &predecessor,
    ])?;
    assert_eq!(replay["created"], false, "{replay}");
    assert_eq!(replay["doc_id"], opening_doc);
    assert_eq!(children_of(&g, &predecessor).await?, children);
    let goal = served.goal(&["show", "--session", &s_goal])?;
    assert_eq!(goal["status"], "active", "{goal}");
    let seq0 = goal["continuation_sequence"]
        .as_u64()
        .context("continuation_sequence")?;

    // The intentional wait: the held turn's bounded wait times out while the job runs.
    wait_for_request_lifecycle_state(&g, &held, &["completed"], Duration::from_secs(60)).await?;
    let waits = wait_for_completed_tool_calls(&g, &s_goal, "wait_process", 1).await?;
    let reply: Value = serde_json::from_str(&canonical_tool_result_text(&g, &waits[0]).await?)?;
    assert_eq!(reply["status"], "running", "{reply}");
    assert_eq!(reply["error"]["reason"], "wait_timeout", "{reply}");
    assert_eq!(
        reply["tool_call_id"].as_str(),
        Some(spawn_handle.lock().unwrap().as_str()),
        "{reply}"
    );
    for _ in 0..20 {
        let children = children_of(&g, &held).await?;
        assert!(
            children.is_empty(),
            "the Goal continued while the job ran: {children:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Release the job; once its row is terminal the held process exits and
    // the completion delivery carries the Goal on.
    released.store(true, Ordering::SeqCst);
    gate.add_permits(1);
    let watch_home = served.home.clone();
    let watch_target = watch_address.lock().unwrap().clone();
    let watch_graphql = g.clone();
    let ended = tokio::task::spawn_blocking(move || {
        let node_home = watch_home.join(".gents");
        let node_home = node_home.to_str().context("gents home is not UTF-8")?;
        let ended = run_cli_json(
            &watch_home,
            &[
                "job",
                "watch",
                &watch_target,
                "--until-terminal",
                "--home",
                node_home,
                "--graphql",
                &watch_graphql,
            ],
        )?;
        fs::OpenOptions::new()
            .write(true)
            .open(&job_ended_pipe)
            .context("releasing the held process")?;
        Ok::<_, anyhow::Error>(ended)
    })
    .await
    .context("job watch worker")??;
    assert_eq!(ended["state"], "completed", "{ended}");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let delivery = loop {
        let children = children_of(&g, &held).await?;
        if let Some(first) = children.first() {
            assert_eq!(children.len(), 1, "{children:?}");
            break first.clone();
        }
        if std::time::Instant::now() >= deadline {
            let state = graphql_query(
                &g,
                r#"{
                    AgentRequest(limit: 100) { _docID request_id session_id lifecycle_state failure_reason caused_by_parent_request_id caused_by_parent_request_doc_id }
                    AgentToolCall(limit: 100) { _docID request_id request_doc_id session_id tool_name tool_call_id lifecycle_state status await_mode tool_failure_class completion_notification_delivered_at }
                    AgentMessage(limit: 100) { _docID session_id request_doc_id message_key sequence role }
                    Goal(limit: 10) { session_id status assignment_root_request_doc_id continuation_sequence last_failure }
                }"#,
            )
            .await;
            let warnings = served._serve.captured_output().map(|(stdout, stderr)| {
                let lines: Vec<_> = stdout
                    .lines()
                    .chain(stderr.lines())
                    .filter(|line| line.contains("WARN") || line.contains("ERROR"))
                    .collect();
                lines[lines.len().saturating_sub(40)..]
                    .iter()
                    .map(|line| line.chars().take(1000).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            let probe_home = served.home.clone();
            let probe_address = watch_address.lock().unwrap().clone();
            let probe_graphql = g.clone();
            let probe = tokio::task::spawn_blocking(move || {
                let node_home = probe_home.join(".gents");
                let node_home = node_home.to_string_lossy();
                run_cli_json(
                    &probe_home,
                    &[
                        "job",
                        "show",
                        &probe_address,
                        "--home",
                        &node_home,
                        "--graphql",
                        &probe_graphql,
                        "--output",
                        "json",
                    ],
                )
            })
            .await;
            panic!("no delivery request after the job ended; durable state: {state:?}; server warnings: {warnings:?}; job show: {probe:?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert!(delivery["caused_by_trigger_kind"].is_null(), "{delivery}");
    assert_eq!(
        delivery["input"]["goal_assignment"],
        json!({"goal_id": resumed["goal_id"], "root_request_doc_id": opening_doc}),
    );
    let delivery_id = delivery["request_id"]
        .as_str()
        .context("delivery request id")?;
    assert!(
        delivery_id.starts_with("background-completion-"),
        "{delivery}"
    );
    wait_for_request_lifecycle_state(&g, delivery_id, &["completed"], Duration::from_secs(60))
        .await?;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let goal = loop {
        let goal = served.goal(&["show", "--session", &s_goal])?;
        if goal["status"] == "complete" {
            break goal;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the Goal did not complete: {goal}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    if goal["continuation_sequence"].as_u64() != Some(seq0 + 1) {
        let requests = graphql_query(&g, &format!(
            "{{AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{_docID request_id purpose lifecycle_state input caused_by_parent_request_id caused_by_trigger_kind created_at}} Goal(filter: {{session_id: {{_eq: \"{}\"}}}}) {{continuation_sequence last_continued_from_request_id assignment_root_request_doc_id token_usage_baseline tokens_used}}}}",
            escape_graphql_string(&served.node_did), escape_graphql_string(&s_goal), escape_graphql_string(&s_goal),
        )).await?;
        panic!(
            "unexpected Goal continuation count: expected {}; goal={goal}; requests={requests}",
            seq0 + 1
        );
    }
    assert!(
        goal["tokens_used"]
            .as_i64()
            .is_some_and(|tokens| tokens > 0),
        "{goal}"
    );
    let updates = wait_for_completed_tool_calls(&g, &s_goal, "update_goal", 1).await?;
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0]["request_doc_id"], delivery["_docID"]);
    assert!(children_of(&g, delivery_id).await?.is_empty());
    assert_eq!(children_of(&g, &held).await?.len(), 1);
    Ok(())
}
