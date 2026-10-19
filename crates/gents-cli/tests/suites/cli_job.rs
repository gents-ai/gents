use crate::support::graphql::{graphql_mutation_with_variables, served_endpoint};
use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::plugin_job::keys::{job_session_id, job_spawn_key};
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::job::{
    JobBudget, JobCeiling, JobOrigin, JobPauseReason, JobPin, JobProgress, JobState,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use serde_json::{json, Value};
use uuid::Uuid;

/// A home served by `gents server`, with the identity its job rows belong to.
pub(crate) struct ServedHome {
    _tempdir: tempfile::TempDir,
    pub(crate) home: PathBuf,
    pub(crate) graphql: String,
    pub(crate) node_did: String,
    _serve: ServeProcess,
}

impl ServedHome {
    pub(crate) async fn start() -> Result<Self> {
        let tempdir = tempfile::tempdir().context("creating tempdir")?;
        let home = tempdir.path().join("home");
        fs::create_dir_all(&home)?;
        let model_name = format!("mock-job-cli-{}", Uuid::new_v4().simple());
        let mock_endpoint = MockModelEndpoint::start(&model_name)?;
        let port = allocate_port()?;
        let graphql = graphql_url(port);
        let init = run_init_json(
            &home,
            &[
                "--node-name",
                &format!("cli-job-{}", Uuid::new_v4().simple()),
                "--model-name",
                &model_name,
                "--inference-url",
                mock_endpoint.endpoint(),
            ],
        )?;
        let node_did = node_did_from_init(&init)?;
        let mut serve = spawn_server(&home, port)?;
        wait_for_port(port, &mut serve)?;
        wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
        Ok(Self {
            _tempdir: tempdir,
            home,
            graphql,
            node_did,
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
            session_id: "{session}", node_did: "{node_did}", tool_name: "workflow",
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
        requester_did: None,
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
        message_key: gents::session::sequence_message_key(node_did, session, None, 1),
        session_id: session.into(),
        node_did: node_did.into(),
        requester_did: None,
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
        requester_did: None,
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
        requester_did: None,
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
        stdout.contains("workflow result job-s1:call-done"),
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
        let request = graphql_mutation_with_variables(
            &access,
            &format!(
                r#"mutation {{ create_AgentRequest(input: {{ request_id: "request-call-running", purpose: "normal", node_did: "{node_did}", agent_id: "general", session_id: "job-s1", content: "run", lifecycle_state: "processing", created_at: "2026-06-01T10:00:00Z" }}) {{ _docID }} }}"#,
                node_did = escape_graphql_string(&did)
            ),
            &json!({}),
        )
        .await?;
        let request_doc_id =
            gents_protocol::graphql::extract_mutation_doc_id(&request, "AgentRequest")?;
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
