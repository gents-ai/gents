use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[path = "../support/failure_artifacts.rs"]
mod failure_artifacts;
use failure_artifacts::FailureArtifacts;

const SEEDED_REPO: &str = "review_seeded_bug";
const CODE_REVIEW_PACK: &str = "gents/code_review";
// The inference slots the code_review manifest declares.
const REVIEW_SLOTS: [&str; 3] = ["coordinator", "worker", "verifier"];
const LENSES: [&str; 4] = [
    "correctness",
    "architecture-reuse",
    "security-concurrency",
    "workflow-invariants",
];
// fixtures/base/src/lib.rs of the packs scenario code_review_live.
const SEEDED_BASE_LIB: &str = r#"//! Pagination helpers.

/// Number of pages needed for `total` items at `per_page` items per page.
/// `per_page` must be at least 1.
pub fn page_count(total: usize, per_page: usize) -> usize {
    (total + per_page - 1) / per_page
}

/// The items on 1-based page `page`, in order.
pub fn page_items<T: Clone>(items: &[T], page: usize, per_page: usize) -> Vec<T> {
    let start = (page - 1) * per_page;
    let end = (start + per_page).min(items.len());
    items[start..end].to_vec()
}
"#;

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    anyhow::ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Builds `<root>/review_seeded_bug` with a base commit and a head commit that
/// adds an off-by-one at `src/lib.rs:12`; returns (base_sha, head_sha).
fn make_seeded_bug_repo(root: &Path) -> Result<(String, String)> {
    let repo = root.join(SEEDED_REPO);
    fs::create_dir_all(repo.join("src"))?;
    fs::write(repo.join("src/lib.rs"), SEEDED_BASE_LIB)?;
    git(&repo, &["init", "-q"])?;
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-qm", "base"])?;
    let base = git(&repo, &["rev-parse", "HEAD"])?;
    fs::write(
        repo.join("src/lib.rs"),
        SEEDED_BASE_LIB.replace("per_page).min", "per_page + 1).min"),
    )?;
    git(&repo, &["commit", "-qam", "head"])?;
    let head = git(&repo, &["rev-parse", "HEAD"])?;
    Ok((base, head))
}

fn job_address_from_list(jobs: &Value, session: &str) -> Result<(String, String)> {
    jobs.as_array()
        .into_iter()
        .flatten()
        .filter_map(|job| job.get("address")?.as_str()?.rsplit_once(':'))
        .find(|(session_id, _)| *session_id == session)
        .map(|(session_id, tool_call_id)| (session_id.to_owned(), tool_call_id.to_owned()))
        .with_context(|| format!("job list has no job of session {session}: {jobs}"))
}

/// The one terminal line of `gents job watch --until-terminal`; any end other
/// than `completed` exits non-zero and fails here with the line.
fn watch_job_until_terminal(home_dir: &Path, address: &str, graphql: &str) -> Result<Value> {
    run_cli_json(
        home_dir,
        &[
            "job",
            "watch",
            address,
            "--until-terminal",
            "--home",
            home_dir.to_str().context("live review home is not UTF-8")?,
            "--graphql",
            graphql,
            "--output",
            "json",
        ],
    )
}

/// A job row stores no `terminal_output`; its delivered result is the row's
/// tool_call output stream, split over `AgentOutputSegment` rows.
fn job_result_from_segments(segments: &Value, row_doc_id: &str) -> Result<Value> {
    let rows = segments
        .as_array()
        .into_iter()
        .flatten()
        .filter(|segment| {
            segment["source"]["kind"] == "tool_call"
                && segment["source"]["tool_call_doc_id"] == row_doc_id
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !rows.is_empty(),
        "no output segments for job row {row_doc_id}: {segments}"
    );
    let closes = rows
        .iter()
        .map(|segment| &segment["close"])
        .filter(|close| !close.is_null())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        closes.len() == 1 && closes[0]["kind"] == "closed" && closes[0]["outcome"] == "complete",
        "job row {row_doc_id} output is not closed exactly once as complete: {closes:?}"
    );
    let mut parts = rows
        .iter()
        .map(|segment| (segment["ordinal"].as_u64(), segment["payload"].as_str()))
        .collect::<Vec<_>>();
    parts.sort();
    let text = parts
        .into_iter()
        .map(|(_, payload)| payload.unwrap_or_default())
        .collect::<String>();
    serde_json::from_str(&text).with_context(|| format!("job result is not JSON: {text}"))
}

async fn job_result(graphql: &str, session: &str, view: &Value) -> Result<Value> {
    let row_doc_id = view["row_doc_id"]
        .as_str()
        .with_context(|| format!("job view has no row_doc_id: {view}"))?;
    let rows = graphql_query(
        graphql,
        &format!(
            r#"{{ AgentOutputSegment(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ ordinal payload source close }} }}"#,
            escape_graphql_string(session)
        ),
    )
    .await?;
    job_result_from_segments(&rows["data"]["AgentOutputSegment"], row_doc_id)
}

/// Each provider turn of a child session as text, oldest first, so the last
/// block is the reply its slot parsed (or rejected).
fn child_turn_texts(segments: &Value) -> String {
    let mut turns = std::collections::BTreeMap::<(&str, u64), Vec<(&str, u64, &str)>>::new();
    for segment in segments.as_array().into_iter().flatten() {
        if segment["source"]["kind"] != "provider_turn" {
            continue;
        }
        let key = (
            segment["request_doc_id"].as_str().unwrap_or_default(),
            segment["source"]["turn_index"].as_u64().unwrap_or_default(),
        );
        turns.entry(key).or_default().push((
            segment["created_at"].as_str().unwrap_or_default(),
            segment["ordinal"].as_u64().unwrap_or_default(),
            segment["payload"].as_str().unwrap_or_default(),
        ));
    }
    let mut turns = turns.into_iter().collect::<Vec<_>>();
    turns.sort_by_key(|(_, parts)| parts.iter().map(|part| part.0).min());
    turns
        .into_iter()
        .map(|((request, index), mut parts)| {
            parts.sort_by_key(|part| part.1);
            let text = parts.iter().map(|part| part.2).collect::<String>();
            format!("--- request {request} turn {index} ---\n{text}\n")
        })
        .collect()
}

async fn write_child_replies(graphql: &str, view: &Value, evidence: &Path) -> Result<()> {
    let dir = evidence.join("children");
    fs::create_dir_all(&dir)?;
    for (index, child) in view["children"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let session = child["session_id"]
            .as_str()
            .with_context(|| format!("job child has no session_id: {child}"))?;
        let rows = graphql_query(
            graphql,
            &format!(
                r#"{{ AgentOutputSegment(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ request_doc_id ordinal created_at payload source }} }}"#,
                escape_graphql_string(session)
            ),
        )
        .await?;
        let title = child["title"]
            .as_str()
            .unwrap_or("child")
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
        fs::write(
            dir.join(format!("{index:02}-{title}.txt")),
            child_turn_texts(&rows["data"]["AgentOutputSegment"]),
        )?;
    }
    Ok(())
}

fn workflow_scratch_path(cwd: &Path, tool_call_id: &str, name: &str) -> PathBuf {
    cwd.join(".gents/workflows/runs")
        .join(tool_call_id)
        .join(name)
}

/// Installs code_review (its `review_changes` job plugin needs
/// `--grant-authority`), then wires the plugin to `agent` with the pack's own
/// targets, which also trusts the pack; no Tools edit. Every declared slot
/// binds to `profile_id`. Packs that ship plugins install only on the node's
/// own host, so this uses `--home`.
fn install_and_wire_workflow_packs(
    home_dir: &Path,
    node_did: &str,
    agent: &str,
    profile_id: &str,
) -> Result<()> {
    let bind = |slots: &[&str]| {
        slots
            .iter()
            .flat_map(|slot| {
                [
                    "--inference-slot".to_owned(),
                    format!("{slot}={profile_id}"),
                ]
            })
            .collect::<Vec<_>>()
    };
    let review_slots = bind(&REVIEW_SLOTS);
    let home = home_dir.to_str().context("live review home is not UTF-8")?;
    let scope = ["--home", home, "--node-did", node_did];
    let content =
        std::env::var("GENTS_CODE_REVIEW_PACK").unwrap_or_else(|_| CODE_REVIEW_PACK.to_owned());
    let mut args = vec!["pack", "install", content.as_str(), "--grant-authority"];
    args.extend(scope);
    args.extend(review_slots.iter().map(String::as_str));
    args.extend(["--output", "json"]);
    run_cli_json(home_dir, &args)?;

    let mut args = vec![
        "pack",
        "wire",
        "gents/review_changes",
        "--agent",
        agent,
        "--targets-from",
        CODE_REVIEW_PACK,
    ];
    args.extend(scope);
    run_cli_json(home_dir, &args)?;
    Ok(())
}

#[test]
fn job_address_splits_at_the_last_colon() -> Result<()> {
    let jobs = serde_json::json!([{ "address": "did:key:z6Mk:session-1:call_7" }]);
    let (session_id, tool_call_id) = job_address_from_list(&jobs, "did:key:z6Mk:session-1")?;
    anyhow::ensure!(session_id == "did:key:z6Mk:session-1" && tool_call_id == "call_7");
    Ok(())
}

#[test]
fn job_address_reads_the_jobview_address_field() -> Result<()> {
    let jobs = serde_json::json!([
        { "address": "other-session:call_1", "session_id": "other-session", "tool_call_id": "call_1", "state": "completed" },
        { "address": "session-2:call_9", "session_id": "session-2", "tool_call_id": "call_9", "state": "running" }
    ]);
    anyhow::ensure!(
        job_address_from_list(&jobs, "session-2")? == ("session-2".to_owned(), "call_9".to_owned())
    );
    anyhow::ensure!(job_address_from_list(&jobs, "absent-session").is_err());
    Ok(())
}

#[test]
fn workflow_scratch_path_names_the_tool_call() {
    assert_eq!(
        workflow_scratch_path(Path::new("/work"), "call_9", "review.md"),
        Path::new("/work/.gents/workflows/runs/call_9/review.md")
    );
}

#[test]
fn job_result_joins_the_job_rows_segments_in_order() -> Result<()> {
    let job = |ordinal: u32, payload: &str| {
        serde_json::json!({ "ordinal": ordinal, "payload": payload,
            "source": { "kind": "tool_call", "tool_call_doc_id": "bae-job" } })
    };
    let closed = |outcome: &str| serde_json::json!({ "kind": "closed", "outcome": outcome, "segments": 2, "stream_bytes": [19] });
    let with_close = |mut segment: Value, close: Value| {
        segment["close"] = close;
        segment
    };
    let segments = serde_json::json!([
        job(1, "\"blocked\"}"),
        { "ordinal": 0, "payload": "{\"other\":1}",
          "source": { "kind": "tool_call", "tool_call_doc_id": "bae-other" } },
        { "ordinal": 0, "payload": "turn text", "source": { "kind": "provider_turn" } },
        with_close(job(0, "{\"status\":"), closed("complete")),
    ]);
    anyhow::ensure!(
        job_result_from_segments(&segments, "bae-job")?
            == serde_json::json!({ "status": "blocked" })
    );
    anyhow::ensure!(job_result_from_segments(&segments, "bae-absent").is_err());
    Ok(())
}

#[test]
fn job_result_requires_exactly_one_complete_closure() -> Result<()> {
    let row = |ordinal: u32, payload: &str, close: Value| {
        serde_json::json!({ "ordinal": ordinal, "payload": payload, "close": close,
            "source": { "kind": "tool_call", "tool_call_doc_id": "bae-job" } })
    };
    let closed = |outcome: &str| serde_json::json!({ "kind": "closed", "outcome": outcome });
    let unclosed = serde_json::json!([row(0, "{}", Value::Null)]);
    let partial = serde_json::json!([row(0, "{}", closed("partial"))]);
    let retracted = serde_json::json!([row(0, "{}", serde_json::json!({ "kind": "retracted" }))]);
    let conflicting = serde_json::json!([
        row(0, "{}", closed("complete")),
        row(1, "", closed("complete"))
    ]);
    for segments in [unclosed, partial, retracted, conflicting] {
        anyhow::ensure!(
            job_result_from_segments(&segments, "bae-job").is_err(),
            "accepted {segments}"
        );
    }
    Ok(())
}

#[test]
fn child_turn_texts_list_each_provider_turn_in_order() {
    let turn = |request: &str, index: u32, ordinal: u32, at: &str, payload: &str| {
        serde_json::json!({ "request_doc_id": request, "ordinal": ordinal, "created_at": at,
            "payload": payload,
            "source": { "kind": "provider_turn", "turn_index": index } })
    };
    let segments = serde_json::json!([
        turn("bae-fix", 0, 0, "2026-10-21T10:00:05Z", "fixed"),
        turn("bae-first", 1, 1, "2026-10-21T10:00:03Z", " reply"),
        { "ordinal": 0, "payload": "tool output", "created_at": "2026-10-21T10:00:02Z",
          "source": { "kind": "tool_call", "tool_call_doc_id": "bae-call" } },
        turn("bae-first", 0, 0, "2026-10-21T10:00:01Z", "call"),
        turn("bae-first", 1, 0, "2026-10-21T10:00:02Z", "bad"),
    ]);
    assert_eq!(
        child_turn_texts(&segments),
        "--- request bae-first turn 0 ---\ncall\n\
         --- request bae-first turn 1 ---\nbad reply\n\
         --- request bae-fix turn 0 ---\nfixed\n"
    );
}

#[test]
fn seeded_bug_repo_changes_one_line() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (base, head) = make_seeded_bug_repo(root.path())?;
    let numstat = git(
        &root.path().join(SEEDED_REPO),
        &["diff", "--numstat", &base, &head],
    )?;
    anyhow::ensure!(
        numstat == "1\t1\tsrc/lib.rs",
        "unexpected seeded diff: {numstat}"
    );
    Ok(())
}

/// A served node with code_review wired to its default agent, plus the
/// directory each case writes its evidence to.
struct ReviewNode {
    home: PathBuf,
    graphql: String,
    node_did: String,
    work: PathBuf,
    evidence: PathBuf,
}

/// One review-changes job as the host left it.
struct ReviewJob {
    tool_call_id: String,
    view: Value,
    result: Value,
}

impl ReviewNode {
    fn cli(&self, args: &[&str]) -> Result<Value> {
        let home = self
            .home
            .to_str()
            .context("live review home is not UTF-8")?;
        let mut args = args.to_vec();
        args.extend([
            "--home",
            home,
            "--graphql",
            &self.graphql,
            "--node-did",
            &self.node_did,
        ]);
        run_cli_json(&self.home, &args)
    }

    /// Asks the default agent to run review-changes once in a fresh session
    /// and returns the job, after watching it to `completed` when `watch`.
    /// A pinned step-0 pause is delivered inline as the call's own reply: an
    /// unwatched job has settled when the launching request ends, and it
    /// delivers no result stream (`result` is null).
    async fn review(&self, case: &str, args: Value, budget: u32, watch: bool) -> Result<ReviewJob> {
        let session = format!("review-workflow-live-{case}-{}", Uuid::new_v4().simple());
        let cwd = self
            .work
            .to_str()
            .context("live review work dir is not UTF-8")?;
        let input = serde_json::json!({ "cwd": cwd }).to_string();
        let content = format!(
            "Call the review_changes tool exactly once with agent_budget {budget} and args {args}. Whatever it returns (a receipt, a pause or an error), report it and end your turn: do not poll, and never call the tool again. A result arrives later as a message."
        );
        let evidence = self.evidence.join(case);
        fs::create_dir_all(&evidence)?;
        let submitted = self.cli(&[
            "request",
            "submit",
            "--session-id",
            &session,
            "--input",
            &input,
            "--content",
            &content,
            "--timeout-secs",
            "600",
        ])?;
        fs::write(
            evidence.join("request.json"),
            serde_json::to_string_pretty(&submitted)?,
        )?;
        let jobs = self.cli(&[
            "job",
            "list",
            "--session",
            &session,
            "--state",
            "all",
            "--output",
            "json",
        ])?;
        anyhow::ensure!(
            jobs.as_array().is_some_and(|jobs| jobs.len() == 1),
            "{case}: expected exactly one job in the session: {jobs}; request: {submitted}"
        );
        let (_, tool_call_id) = job_address_from_list(&jobs, &session)?;
        let address = format!("{session}:{tool_call_id}");
        if watch {
            let ended = watch_job_until_terminal(&self.home, &address, &self.graphql)?;
            fs::write(evidence.join("watch.json"), ended.to_string())?;
            anyhow::ensure!(
                ended["state"] == "completed" && ended["address"] == address.as_str(),
                "{case}: the job did not complete: {ended}"
            );
        }
        let view = self.cli(&["job", "show", &address, "--output", "json"])?;
        fs::write(
            evidence.join("job.json"),
            serde_json::to_string_pretty(&view)?,
        )?;
        write_child_replies(&self.graphql, &view, &evidence).await?;
        let result = if watch {
            job_result(&self.graphql, &session, &view).await?
        } else {
            Value::Null
        };
        fs::write(
            evidence.join("result.json"),
            serde_json::to_string_pretty(&result)?,
        )?;
        let report = workflow_scratch_path(&self.work, &tool_call_id, "review.md");
        if report.exists() {
            fs::copy(&report, evidence.join("review.md"))?;
        }

        let pin = &view["pin"];
        anyhow::ensure!(
            pin["origin"] == "installed",
            "{case}: the trusted pack's marker did not make the run installed: {view}"
        );
        anyhow::ensure!(
            pin["agent_budget"] == budget && pin["cwd"] == cwd,
            "{case}: the pin lost the call's budget or cwd: {view}"
        );
        anyhow::ensure!(
            pin["plugin_ref"]
                .as_str()
                .is_some_and(|plugin| plugin.contains("gents/review_changes"))
                && pin["script_id"]
                    .as_str()
                    .is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())),
            "{case}: the pin does not name the review_changes plugin and a marker digest: {view}"
        );
        Ok(ReviewJob {
            tool_call_id,
            view,
            result,
        })
    }
}

impl ReviewJob {
    fn titles(&self) -> Vec<&str> {
        let mut titles = self.view["children"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|child| child["title"].as_str())
            .collect::<Vec<_>>();
        titles.sort_unstable();
        titles
    }

    fn count(&self, field: &str) -> u64 {
        self.result["counts"][field].as_u64().unwrap_or(u64::MAX)
    }

    fn report(&self, node: &ReviewNode) -> Result<String> {
        anyhow::ensure!(
            self.result["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(&format!(
                    ".gents/workflows/runs/{}/review.md",
                    self.tool_call_id
                ))),
            "result path is not the tool_call_id scratch path: {}",
            self.result
        );
        let report = workflow_scratch_path(&node.work, &self.tool_call_id, "review.md");
        fs::read_to_string(&report).with_context(|| format!("reading {}", report.display()))
    }

    /// The early terminals (blocked recon, no changed paths) end after recon
    /// alone, with zero counts and no findings.
    fn ensure_recon_only_terminal(&self) -> Result<()> {
        anyhow::ensure!(
            self.titles() == ["recon"] && self.view["children"][0]["state"] == "completed",
            "an early terminal must follow recon alone: {}",
            self.view
        );
        anyhow::ensure!(
            [
                "candidate",
                "confirmed",
                "refuted",
                "unverified",
                "high_priority"
            ]
            .iter()
            .all(|field| self.count(field) == 0)
                && self.result["findings"] == serde_json::json!([])
                && self.result["report"]["merge_recommendation"] == "none",
            "an early terminal must report nothing: {}",
            self.result
        );
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs review-changes against a real model and consumes real tokens"]
async fn review_changes_workflow_runs_live_through_each_terminal() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cli_live_suite=info".into()),
        )
        .with_test_writer()
        .try_init();
    let root = std::env::var_os("GENTS_REVIEW_WORKFLOW_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gents-review-workflow-failures"));
    let retain = std::env::var_os("GENTS_REVIEW_WORKFLOW_RETAIN_FAILURES").as_deref()
        != Some(std::ffi::OsStr::new("0"));
    let evidence = root.join(format!("evidence-{}", Uuid::new_v4().simple()));
    let mut artifacts = FailureArtifacts::new(&root, retain)?;
    let result = run_review_changes(&mut artifacts, &evidence).await;
    tracing::info!(evidence = %evidence.display(), "review-changes live evidence");
    artifacts.finish(result)
}

async fn run_review_changes(artifacts: &mut FailureArtifacts, evidence: &Path) -> Result<()> {
    let home_dir = artifacts.path().join("agent-home");
    fs::create_dir_all(&home_dir)?;
    let home_arg = home_dir.to_str().context("live review home is not UTF-8")?;
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .context("GENTS_CLI_E2E_MODEL_ENDPOINT must identify a real model endpoint")?;
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .context("GENTS_CLI_E2E_MODEL_NAME must identify a real model")?;
    let node_name = format!("review-workflow-live-{}", Uuid::new_v4().simple());
    let mut init_args = vec![
        "--home",
        home_arg,
        "--node-name",
        &node_name,
        "--model-name",
        &model_name,
        "--max-concurrent",
        "6",
        "--max-queue-depth",
        "16",
        "--inference-url",
        &model_endpoint,
    ];
    if std::env::var("GENTS_CLI_E2E_API_KEY").is_ok_and(|api_key| !api_key.trim().is_empty()) {
        init_args.extend(["--api-key-env-var", "GENTS_CLI_E2E_API_KEY"]);
    }
    let init = run_init_json(&home_dir, &init_args)?;
    let node_did = node_did_from_init(&init)?;
    let agent = init["default_agent_id"]
        .as_str()
        .context("init receipt is missing default_agent_id")?;
    let profile_id = init["inference_profile_id"]
        .as_str()
        .context("init receipt is missing inference_profile_id")?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (serve, readiness) = crate::support::process::spawn_server_with_ready_json_in(
        &home_dir,
        port,
        &["--home", home_arg],
        &[("RUST_LOG", "warn")],
        artifacts.path(),
    )?;
    artifacts.attach_server(serve);
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "Gents server did not become ready: {readiness}"
    );
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    install_and_wire_workflow_packs(&home_dir, &node_did, agent, profile_id)?;
    wait_for_runtime_quiescence(&graphql, &node_did, 2, Duration::from_secs(6)).await?;

    // Request admission keeps a request's cwd inside the node's tool root.
    let work = home_dir.join("work");
    fs::create_dir_all(&work)?;
    let work = fs::canonicalize(&work)?;
    let (base_sha, head_sha) = make_seeded_bug_repo(&work)?;
    let node = ReviewNode {
        home: home_dir.clone(),
        graphql,
        node_did,
        work,
        evidence: evidence.to_path_buf(),
    };
    let args = |base: &str, head: &str| serde_json::json!({ "repository": SEEDED_REPO, "base": base, "head": head });

    // Step 0: below 22 the plugin pauses before any child runs.
    let paused = node
        .review("budget-pause", args("HEAD~1", "HEAD"), 8, false)
        .await?;
    anyhow::ensure!(
        paused.view["state"] == "paused"
            && paused.view["progress"]["pause_reason"] == "budget_exceeded"
            && paused.view["progress"]["spawned"] == 0
            && paused.titles().is_empty(),
        "an agent_budget of 8 must pause budget_exceeded with no children: {} {}",
        paused.view,
        paused.result
    );

    let blocked = node
        .review(
            "blocked-recon",
            args("refs/heads/absent-base", "HEAD"),
            32,
            true,
        )
        .await?;
    blocked.ensure_recon_only_terminal()?;
    let reason = blocked.result["reason"].as_str().unwrap_or_default();
    anyhow::ensure!(
        blocked.result["status"] == "blocked"
            && !reason.is_empty()
            && blocked.result["limitations"] == serde_json::json!([reason])
            && blocked.report(&node)?.starts_with("# Review\n\nBlocked: "),
        "an unresolvable base must block after recon: {}",
        blocked.result
    );

    let empty = node
        .review("empty-diff", args("HEAD", "HEAD"), 32, true)
        .await?;
    empty.ensure_recon_only_terminal()?;
    anyhow::ensure!(
        empty.result["status"] == "complete"
            && empty.result["base_sha"] == head_sha.as_str()
            && empty.result["head_sha"] == head_sha.as_str()
            && empty.result["limitations"] == serde_json::json!([])
            && empty.report(&node)?.contains("No changed paths"),
        "an empty diff must complete after recon with no changed paths: {}",
        empty.result
    );

    let seeded = node
        .review("seeded-bug", args("HEAD~1", "HEAD"), 32, true)
        .await?;
    let verifiers = seeded
        .titles()
        .into_iter()
        .filter(|title| title.starts_with("verify-"))
        .count();
    let mut expected = vec!["recon".to_owned(), "triage".to_owned()];
    expected.extend(LENSES.map(|lens| format!("scan-{lens}")));
    let staged = seeded
        .titles()
        .into_iter()
        .filter(|title| !title.starts_with("verify-"))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    let children = seeded.view["children"]
        .as_array()
        .context("job view has no children")?;
    anyhow::ensure!(
        staged == expected
            && verifiers <= 16
            && children.iter().all(|child| child["state"] == "completed"),
        "expected recon, four scans, at most 16 verifiers and triage, each completed: {}",
        seeded.view
    );
    let result = &seeded.result;
    anyhow::ensure!(
        result["status"] == "complete"
            && result["base_sha"] == base_sha.as_str()
            && result["head_sha"] == head_sha.as_str()
            && result["diff_id"].as_str().is_some_and(|id| !id.is_empty()),
        "review did not complete on the pinned change: {result}"
    );
    anyhow::ensure!(
        seeded.count("candidate")
            == seeded.count("confirmed") + seeded.count("refuted") + seeded.count("unverified"),
        "review counts do not balance: {result}"
    );
    anyhow::ensure!(
        result["findings"].as_array().map(Vec::len) == Some(seeded.count("confirmed") as usize)
            && result["report"]["merge_recommendation"]
                .as_str()
                .is_some_and(|r| !r.is_empty()),
        "findings or the merge recommendation disagree with the counts: {result}"
    );
    anyhow::ensure!(
        (seeded.count("candidate") > 0) == (verifiers > 0),
        "verifier children do not match the candidate count: {result} {}",
        seeded.view
    );
    let limitations = result["limitations"]
        .as_array()
        .context("result has no limitations array")?;
    anyhow::ensure!(
        !limitations
            .iter()
            .filter_map(Value::as_str)
            .any(|entry| entry == "triage did not succeed"
                || LENSES
                    .iter()
                    .any(|lens| entry.starts_with(&format!("{lens}:")))),
        "triage failed or a review lens was blocked, so the review is not clean: {result}"
    );
    anyhow::ensure!(
        seeded.report(&node)?.contains(&head_sha),
        "scratch report does not name the reviewed head"
    );

    let seeded_finding = result["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|finding| {
            finding.to_string().contains("src/lib.rs") && finding.to_string().contains("12")
        });
    tracing::info!(seeded_finding, counts = %result["counts"], "review-changes live result");
    let (stdout, stderr) = artifacts.captured_output()?;
    fs::write(evidence.join("server.stdout"), stdout)?;
    fs::write(evidence.join("server.stderr"), stderr)?;
    Ok(())
}
