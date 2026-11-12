use crate::support::graphql::{graphql_mutation_with_variables, served_endpoint};
use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use gents_protocol::job::keys::{job_session_id, job_spawn_key};
use gents_protocol::job::{
    JobBudget, JobCeiling, JobOrigin, JobPauseReason, JobPin, JobProgress, JobState,
};
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) struct ServedHome {
    _tempdir: tempfile::TempDir,
    pub(crate) home: PathBuf,
    pub(crate) graphql: String,
    pub(crate) node_did: String,
    _model: MockModelEndpoint,
    _serve: ServeProcess,
}

impl ServedHome {
    pub(crate) async fn start() -> Result<Self> {
        let served = tokio::task::spawn_blocking(|| {
            let tempdir = tempfile::tempdir().context("creating tempdir")?;
            let home = tempdir.path().join("home");
            fs::create_dir_all(&home)?;
            let model_name = format!("mock-job-cli-{}", Uuid::new_v4().simple());
            let model = MockModelEndpoint::start(&model_name)?;
            let port = allocate_port()?;
            let graphql = graphql_url(port);
            let node_name = format!("cli-job-{}", Uuid::new_v4().simple());
            let init = run_init_json(
                &home,
                &[
                    "--node-name",
                    &node_name,
                    "--model-name",
                    &model_name,
                    "--inference-url",
                    model.endpoint(),
                ],
            )?;
            let node_did = node_did_from_init(&init)?;
            let mut serve = spawn_server(&home, port)?;
            wait_for_port(port, &mut serve)?;
            Ok::<_, anyhow::Error>(Self {
                _tempdir: tempdir,
                home,
                graphql,
                node_did,
                _model: model,
                _serve: serve,
            })
        })
        .await??;
        wait_for_runtime_ready(&served.graphql, &served.node_did, Duration::from_secs(30)).await?;
        Ok(served)
    }

    pub(crate) fn access(&self) -> ConfigAccess {
        ConfigAccess::Graphql(served_endpoint(&self.graphql))
    }

    pub(crate) async fn job(&self, args: &[&str]) -> Result<(Option<i32>, String, String)> {
        let mut full = vec!["automation".to_owned(), "job".to_owned()];
        full.extend(args.iter().map(|arg| (*arg).to_owned()));
        full.extend(["--graphql".to_owned(), self.graphql.clone()]);
        let home = self.home.clone();
        tokio::task::spawn_blocking(move || {
            run_job(&home, &full.iter().map(String::as_str).collect::<Vec<_>>())
        })
        .await?
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
            agent_did: "default", session_id: "{session}", retry_key: "{retry}",
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

    let (code, stdout, stderr) = served
        .job(&["list", "--state", "all", "--output", "json"])
        .await?;
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

    let (code, stdout, stderr) = served.job(&["show", "job-s1:call-done"]).await?;
    assert_eq!(code, Some(0), "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("completed"), "answer first: {stdout}");
    assert!(
        stdout.contains("children:") && stdout.contains("alpha") && stdout.contains("beta"),
        "{stdout}"
    );
    assert!(
        stdout.contains("gents automation job show job-s1:call-done"),
        "{stdout}"
    );

    for (address, expected_code, state) in [
        ("job-s1:call-done", 0, "completed"),
        ("job:s2:call-paused", 3, "paused"),
    ] {
        let (code, stdout, stderr) = served
            .job(&["watch", address, "--until-terminal", "--output", "json"])
            .await?;
        assert_eq!(code, Some(expected_code), "{address}: {stderr}");
        assert_eq!(stdout.lines().count(), 1, "{stdout}");
        let line: Value = serde_json::from_str(stdout.trim())?;
        assert_eq!(line["address"], address);
        assert_eq!(line["state"], state);
    }

    let (code, _, stderr) = served.job(&["stop", "job-s1:call-done"]).await?;
    assert_ne!(code, Some(0));
    assert!(!stderr.is_empty(), "expected a control error");
    let (_, stdout, _) = served.job(&["show", "job-s1:call-done"]).await?;
    assert!(
        stdout.starts_with("completed"),
        "stop changed the row: {stdout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_access_refuses_watch_and_control_on_an_offline_home() -> Result<()> {
    tokio::task::spawn_blocking(|| {
        let tempdir = tempfile::tempdir().context("creating tempdir")?;
        let node_home = tempdir.path().join("node-home");
        // The refusal is decided by the access kind before any job lookup.
        let init = run_init_json(
            tempdir.path(),
            &[
                "--node-name",
                "cli-job-offline",
                "--home",
                node_home.to_str().context("node home utf8")?,
            ],
        )?;
        node_did_from_init(&init)?;
        let home = node_home.to_str().context("node home utf8")?;

        let (code, _, stderr) = run_job(
            tempdir.path(),
            &[
                "automation",
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
            &[
                "automation",
                "job",
                "stop",
                "job-s1:call-running",
                "--home",
                home,
            ],
        )?;
        assert_ne!(code, Some(0), "{stdout} {stderr}");
        assert!(stderr.contains("start the hosting runtime"), "{stderr}");
        Ok(())
    })
    .await?
}
