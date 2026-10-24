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

#[path = "support/console_io.rs"]
mod console_io;

#[path = "support/console.rs"]
#[cfg(windows)]
mod console;
#[cfg(windows)]
use console::ConsoleChild as Child;

#[path = "support/configured_actions.rs"]
mod configured_actions;
#[path = "support/editor_goal_recovery.rs"]
mod editor_goal_recovery;
#[path = "support/editor_goal_removal.rs"]
mod editor_goal_removal;
#[path = "support/editor_goal_replacement.rs"]
mod editor_goal_replacement;
#[path = "support/editor_history.rs"]
mod editor_history;
#[path = "support/editor_inventory.rs"]
mod editor_inventory;
#[path = "support/editor_inventory_permissions.rs"]
mod editor_inventory_permissions;
#[path = "support/editor_related_work.rs"]
mod editor_related_work;
#[path = "support/editor_retry.rs"]
mod editor_retry;
#[path = "support/editor_review.rs"]
mod editor_review;
#[path = "support/editor_workspace.rs"]
mod editor_workspace;
#[allow(dead_code)]
#[path = "../../gents-cli/tests/support/mocks/fake_llm.rs"]
mod fake_llm;
#[path = "support/initialization.rs"]
mod initialization;
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
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorSteering,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorHistory,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorGoalRecovery,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorGoalRemoval,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorGoalReplacement,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorRetry,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorReview,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorReviewPermissions,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorInventory,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorWorkspace,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorRelatedWork,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    EditorRelatedWorkFailure,
    Goal,
}

impl Journey {
    fn editor_scenario(self) -> Option<&'static str> {
        match self {
            Self::Editor => Some("full"),
            Self::EditorSteering => Some("steering"),
            Self::EditorHistory => Some("history"),
            Self::EditorGoalRecovery => Some("goals"),
            Self::EditorGoalRemoval => Some("goal-removal"),
            Self::EditorGoalReplacement => Some("goal-replacement"),
            Self::EditorRetry => Some("retry"),
            Self::EditorReview => Some("review"),
            Self::EditorReviewPermissions => Some("review-permissions"),
            Self::EditorInventory => Some("review-inventory"),
            Self::EditorWorkspace => Some("workspace"),
            Self::EditorRelatedWork => Some("related-work"),
            Self::EditorRelatedWorkFailure => Some("related-work-failure"),
            _ => None,
        }
    }
}

const STEERING_INPUT: &str = "Steering input from the installed editor.";
const STEERING_REPLY: &str = "Steering input received.";

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

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_steers_pending_input_into_the_active_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorSteering)
        .await
        .with_context(|| {
            std::fs::read_to_string(temp.path().join("editor.log")).unwrap_or_default()
        })?;
    copy_editor_evidence(temp.path(), "editor-steering-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_pages_native_history_without_losing_the_reader_or_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let evidence = editor_history::ProfileEvidence::from_env(temp.path())?;
    let result = exercise(temp.path(), Journey::EditorHistory)
        .await
        .and_then(|()| copy_editor_evidence(temp.path(), "editor-history-canonical.json"));
    if let Some(evidence) = evidence {
        evidence.finish(temp.path(), &result)?;
    }
    result.with_context(|| {
        [
            "runtime.log",
            "runtime.err",
            "client-host.log",
            "editor.log",
            "editor-history-seed-progress.json",
            "editor-history-seed-failure.json",
        ]
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
async fn editor_preserves_captured_context_in_a_multi_root_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorWorkspace)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-workspace-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_retries_failed_input_without_submitting_the_unsent_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorRetry)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-retry-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_reviews_retained_file_images_without_reading_the_working_copy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorReview)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-review-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_pages_real_file_captures_across_reload_and_new_activity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    if let Err(error) = exercise(temp.path(), Journey::EditorInventory).await {
        let mut logs = Vec::new();
        for name in [
            "runtime.log",
            "runtime.err",
            "client-host.log",
            "editor.log",
        ] {
            let contents = tokio::fs::read_to_string(temp.path().join(name))
                .await
                .unwrap_or_default();
            logs.push(format!("{name}:\n{contents}"));
        }
        return Err(error.context(logs.join("\n")));
    }
    let root = temp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        copy_editor_evidence(&root, "editor-inventory-canonical.json")
    })
    .await?
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_retries_recorded_file_review_after_reenrollment() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorReviewPermissions)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-review-permissions-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_recovers_blocked_and_limited_goals_without_submitting_the_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorGoalRecovery)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-goal-recovery-canonical.json")
}

#[cfg(feature = "editor-e2e")]
fn copy_editor_evidence(root: &Path, name: &str) -> Result<()> {
    let artifacts = std::fs::read_to_string(root.join("editor-artifacts-path"))?;
    let artifacts = std::fs::canonicalize(artifacts)?;
    let extension = std::env::var_os("GENTS_VSCODE_ROOT").context("extension root")?;
    ensure!(
        artifacts.starts_with(std::fs::canonicalize(
            Path::new(&extension).join("test-results")
        )?),
        "editor evidence left its artifact directory"
    );
    std::fs::copy(root.join(name), artifacts.join(name))?;
    Ok(())
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_removes_goals_without_cancelling_current_work_or_queued_input() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorGoalRemoval)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-goal-removal-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_replaces_goals_in_the_same_chat_without_losing_work_or_drafts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorGoalReplacement)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-goal-replacement-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_observes_concurrent_related_work_without_losing_the_parent_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorRelatedWork)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-related-work-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_observes_related_worker_failure_without_losing_completed_work_or_the_parent_draft(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::EditorRelatedWorkFailure)
        .await
        .with_context(|| {
            [
                "runtime.log",
                "runtime.err",
                "client-host.log",
                "editor.log",
            ]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
        })?;
    copy_editor_evidence(temp.path(), "editor-related-work-canonical.json")
}

async fn exercise(root: &Path, journey: Journey) -> Result<()> {
    let history = matches!(journey, Journey::EditorHistory)
        .then(editor_history::Options::from_env)
        .transpose()?;
    let cli = support::native_binary("gents")?;
    let host_binary = support::native_binary("gents-client-host")?;
    if journey.editor_scenario().is_some() {
        std::fs::write(
            root.join("AGENTS.md"),
            "# Project instructions fixture\nUse the current editor workspace.\n",
        )?;
    }
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (emitted, _) = tokio::sync::watch::channel(None);
    let provider_gate = gate.clone();
    let editor_goal_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let goal_provider_gate = editor_goal_gate.clone();
    let goal_provider_hold = root.join("hold-goal-provider");
    let retry_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_retry_attempts = Arc::clone(&retry_attempts);
    let goal_recovery = Arc::new(editor_goal_recovery::Provider::default());
    let provider_goal_recovery = Arc::clone(&goal_recovery);
    let goal_removal = Arc::new(editor_goal_removal::Provider::default());
    let provider_goal_removal = Arc::clone(&goal_removal);
    let goal_replacement = Arc::new(editor_goal_replacement::Provider::default());
    let provider_goal_replacement = Arc::clone(&goal_replacement);
    let editor_history_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let history_provider_gate = editor_history_gate.clone();
    let history_provider_hold = root.join("hold-history-provider");
    let related_provider = Arc::new(editor_related_work::Provider::new(matches!(
        journey,
        Journey::EditorRelatedWorkFailure
    )));
    let provider_related = Arc::clone(&related_provider);
    let provider = FakeLlm::start_with_models(
        if journey.editor_scenario().is_some() {
            &["editor-fixture", "editor-fixture-alternate"]
        } else {
            &["editor-fixture"]
        },
        None,
        Arc::new(move |request| {
            if matches!(
                journey,
                Journey::EditorRelatedWork | Journey::EditorRelatedWorkFailure
            ) {
                if let Some(action) = provider_related.reply(request) {
                    return action;
                }
            }
            if matches!(journey, Journey::EditorGoalReplacement) {
                if let Some(action) = provider_goal_replacement.reply(request, &emitted) {
                    return action;
                }
            }
            if matches!(journey, Journey::EditorGoalRemoval) {
                if let Some(action) = provider_goal_removal.reply(request, &emitted) {
                    return action;
                }
            }
            if matches!(journey, Journey::EditorGoalRecovery) {
                if let Some(action) = provider_goal_recovery.reply(request, &emitted) {
                    return action;
                }
            }
            if matches!(journey, Journey::EditorInventory) {
                if let Some(action) = editor_inventory::reply(request) {
                    return action;
                }
            }
            if matches!(
                journey,
                Journey::EditorReview | Journey::EditorReviewPermissions
            ) {
                if let Some(action) = editor_review::reply(request) {
                    return action;
                }
            }
            if matches!(journey, Journey::EditorRetry) {
                if let Some(action) = editor_retry::reply(request, &provider_retry_attempts) {
                    return action;
                }
            }
            if let Some(options) = history {
                if let Some(reply) = editor_history::reply(request, options) {
                    return reply;
                }
            }
            if matches!(journey, Journey::EditorWorkspace)
                && request["messages"]
                    .to_string()
                    .contains(editor_workspace::CAPTURED.trim())
                && !request["messages"]
                    .to_string()
                    .contains(editor_workspace::PROMPT)
            {
                return ChatAction::Sse(fake_llm::completion_text_sse(
                    editor_workspace::ATTACHMENT_ONLY_REPLY,
                ));
            }
            if matches!(journey, Journey::EditorWorkspace)
                && request["messages"]
                    .to_string()
                    .contains(editor_workspace::PROMPT)
            {
                return ChatAction::Sse(fake_llm::completion_text_sse(editor_workspace::REPLY));
            }
            if matches!(journey, Journey::EditorSteering)
                && request["messages"].to_string().contains(STEERING_INPUT)
            {
                return ChatAction::Sse(fake_llm::completion_text_sse(STEERING_REPLY));
            }
            ChatAction::GatedSse(
                vec![(
                    Duration::ZERO,
                    format!(
                        "data: {}\n\n",
                        json!({"choices":[{"delta":{"content":"First part"},"finish_reason":null}],"usage":null})
                    ),
                )],
                if history_provider_hold.is_file() {
                    history_provider_gate.clone()
                } else if goal_provider_hold.is_file() {
                    goal_provider_gate.clone()
                } else {
                    provider_gate.clone()
                },
                fake_llm::completion_text_sse(" and final part."),
                emitted.clone(),
            )
        }),
    )?;
    let node_home = root.join("runtime");
    let mut init = Command::new(&cli);
    init.current_dir(root)
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["init", "--store-key-custody", "file", "--home"])
        .arg(&node_home)
        .args([
            "--tool-package",
            if journey.editor_scenario().is_some() {
                "write"
            } else {
                "readonly"
            },
            "--tool-root",
        ])
        .arg(root)
        .args([
            "--node-name",
            "editor-fixture",
            "--model-name",
            "editor-fixture",
            "--inference-url",
        ])
        .arg(provider.endpoint());
    let output = initialization::Initialization::spawn(&mut init, root)?
        .wait(Duration::from_secs(30))
        .await?;
    let init: Value = serde_json::from_slice(&output)?;
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
    let mut host_command = Command::new(&host_binary);
    host_command.args(["--serve", "--home"]).arg(paths.root());
    if journey.editor_scenario().is_some() {
        host_command.arg("--editor-store");
    }
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
    let receipt = if matches!(journey, Journey::Goal | Journey::EditorGoalRecovery) {
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
                    "nodeDid":node_did,"agentId":agent_id,"content":content,"delivery":"queue",
                    "cwd": matches!(journey, Journey::EditorSteering).then_some(root)
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
    let original_doc_id = row.doc_id.clone().context("submitted physical request")?;
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
    if matches!(journey, Journey::EditorSteering) {
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
        run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "steering",
        )
        .await?;
        id += 1;
    }
    if matches!(journey, Journey::EditorGoalRecovery) {
        wait_goal(&mut client, &mut id, node_did, session, "active").await?;
    }
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
        let context = access.execute(&format!(
            r#"{{
                AgentMessage(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ message_key request_doc_id }}
                AgentOutputSegment(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ payload }}
            }}"#,
            gents::graphql::escape_graphql_string(session),
            gents::graphql::escape_graphql_string(session),
        )).await?;
        ensure!(
            context["data"]["AgentMessage"]
                .as_array()
                .is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message["request_doc_id"].as_str().is_some_and(|doc_id| {
                            message["message_key"] == format!("authored:{doc_id}:context")
                        })
                    })
                })
                && context["data"]["AgentOutputSegment"]
                    .to_string()
                    .contains("Project instructions fixture"),
            "the fixture did not preserve its native workspace instructions: {context}"
        );
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
        let goal_completion_path = root.join("finish-goal-provider");
        let _goal_completion = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                if goal_completion_path.is_file() {
                    editor_goal_gate.add_permits(1);
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
        let result = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "full",
        )
        .await;
        if result.is_err() {
            approval_wait.abort();
        }
        result?;
        let stopped = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ node_did: {{_eq: "{}"}}, purpose: {{_eq: "normal"}} }}) {{ content session_id lifecycle_state }} }}"#,
                gents::graphql::escape_graphql_string(node_did),
            ))
            .await?;
        let stopped = stopped["data"]["AgentRequest"]
            .as_array()
            .context("stopped editor request rows")?
            .iter()
            .filter(|row| {
                matches!(
                    row["content"].as_str(),
                    Some(
                        "Verify the installed goal controls" | "Continue with the queued follow-up"
                    )
                )
            })
            .collect::<Vec<_>>();
        ensure!(
            stopped.len() == 2
                && stopped[0]["content"] != stopped[1]["content"]
                && stopped[0]["session_id"] == stopped[1]["session_id"]
                && stopped.iter().all(|row| row["lifecycle_state"] == "interrupted"),
            "both editor Stop actions must reach distinct requests in the same session: {stopped:?}"
        );
        configured_actions::verify_editor_settings(&access, node_did, agent_id).await?;
        let submitted = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{_eq: "{}"}}, purpose: {{_eq: "normal"}} }}) {{ content input }} }}"#,
            gents::graphql::escape_graphql_string(node_did),
        )).await?;
        let rows = submitted["data"]["AgentRequest"]
            .as_array()
            .context("editor workspace request")?
            .iter()
            .filter(|row| {
                row["content"]
                    .as_str()
                    .is_some_and(|content| content.starts_with("Check this repository"))
            })
            .collect::<Vec<_>>();
        ensure!(
            rows.len() == 1,
            "the editor must submit one workspace request: {submitted}"
        );
        let selected = client
            .call(
                id,
                "client_inference_setup_state",
                json!({"nodeDid":node_did}),
            )
            .await;
        id += 1;
        let binding = selected["agentConfigs"]
            .as_array()
            .and_then(|agents| agents.iter().find(|agent| agent["agent_id"] == agent_id))
            .context("selected agent configuration")?;
        ensure!(
            selected["inferenceProfiles"]
                .as_array()
                .is_some_and(|profiles| {
                    profiles.iter().any(|profile| {
                        profile["profile_id"] == binding["inference_profile_id"]
                            && profile["model_name"] == "editor-fixture-alternate"
                            && profile["service_tier"] == "fast"
                    })
                }),
            "the composer model selection did not reach the selected agent: {selected}"
        );
        ensure!(
            provider.captured_chat_requests().iter().any(|request| {
                request["model"] == "editor-fixture-alternate"
                    && request["service_tier"] == "priority"
                    && request["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| {
                            message["role"] == "user"
                                && match &message["content"] {
                                    Value::String(text) => text.contains("Check this repository"),
                                    Value::Array(parts) => parts.iter().any(|part| {
                                        part["text"].as_str().is_some_and(|text| {
                                            text.contains("Check this repository")
                                        })
                                    }),
                                    _ => false,
                                }
                        })
                    })
            }),
            "the editor request did not use the selected model"
        );
        let cwd = rows[0]["input"]["cwd"]
            .as_str()
            .context("the editor request has no workspace folder")?;
        ensure!(
            std::fs::canonicalize(cwd)? == std::fs::canonicalize(root.join("second-workspace"))?,
            "the editor request used a different project folder"
        );
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
    let expected_reply = if matches!(journey, Journey::EditorSteering) {
        STEERING_REPLY
    } else {
        "First part and final part."
    };
    let mut last_projection = Value::Null;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let projection = read_session(&mut client, &mut id, node_did, session).await?;
            let durable = projection["timelineItems"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["kind"] == "assistantMessage" && item["content"] == expected_reply
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
                    ensure!(output["output"]["content"] == expected_reply, "{output}");
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
    if matches!(journey, Journey::EditorGoalRecovery) {
        let blocked =
            editor_goal_recovery::wait_for_source_blocked(&access, node_did, session).await?;
        wait_goal(&mut client, &mut id, node_did, session, "blocked").await?;
        std::fs::write(
            root.join("editor-goal-blocked-source.json"),
            serde_json::to_vec_pretty(&blocked)?,
        )?;
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
        user_rounds
            == if matches!(journey, Journey::EditorGoalRecovery) {
                7
            } else if matches!(journey, Journey::EditorSteering) {
                2
            } else {
                1
            },
        "unexpected provider round count: {user_rounds}"
    );
    if matches!(journey, Journey::EditorSteering) {
        let evidence = verify_editor_steering(
            &access,
            &provider,
            root,
            node_did,
            session,
            &original_doc_id,
            &client_did,
        )
        .await?;
        std::fs::write(
            root.join("editor-steering-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if matches!(journey, Journey::EditorHistory) {
        let options = history.context("history options")?;
        let evidence = editor_history::populate(
            &mut client,
            &mut id,
            &access,
            node_did,
            session,
            agent_id,
            &client_did,
            options,
            root,
        )
        .await?;
        std::fs::write(
            root.join("editor-history-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let completion_path = root.join("finish-history-provider");
        if options.profile {
            std::fs::write(root.join("hold-history-provider"), "")?;
            #[cfg(unix)]
            let runtime_pid = runtime.id();
            #[cfg(windows)]
            let runtime_pid: Option<u32> = None;
            std::fs::write(
                root.join("editor-history-processes.json"),
                serde_json::to_vec_pretty(&json!({
                    "runtimePid":runtime_pid, "fixturePid":std::process::id(),
                    "runtimeBinary":cli, "hostBinary":host_binary,
                }))?,
            )?;
        }
        let _completion = options.profile.then(|| {
            tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                loop {
                    if completion_path.is_file() {
                        editor_history_gate.add_permits(1);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }))
        });
        run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "history",
        )
        .await?;
        if options.profile {
            let evidence = editor_history::verify_profile(
                &mut client,
                &mut id,
                &access,
                node_did,
                session,
                &client_did,
                options,
            )
            .await?;
            std::fs::write(
                root.join("editor-history-profile-canonical.json"),
                serde_json::to_vec_pretty(&evidence)?,
            )?;
            #[cfg(feature = "editor-e2e")]
            copy_editor_evidence(root, "editor-history-profile-canonical.json")?;
        }
    }
    if matches!(
        journey,
        Journey::EditorRelatedWork | Journey::EditorRelatedWorkFailure
    ) {
        editor_related_work::configure(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let result = tokio::try_join!(
            run_editor_journey(
                root,
                &paths,
                &host_binary,
                session,
                provider.endpoint(),
                journey
                    .editor_scenario()
                    .expect("related-work editor scenario")
            ),
            related_provider.release_when_requested(root),
        );
        if let Err(error) = result {
            let snapshot = client.snapshot(id).await;
            let provenance = client.call(id + 1, "desktop_session_provenance", json!({"request":{"nodeDid":node_did,"sessionId":session,"requesterDid":client_did}})).await;
            anyhow::bail!("{error:#}; related-work snapshot: {snapshot}; provenance: {provenance}");
        }
        let evidence = editor_related_work::verify(
            &access,
            node_did,
            session,
            &client_did,
            root,
            matches!(journey, Journey::EditorRelatedWorkFailure),
        )
        .await?;
        std::fs::write(
            root.join("editor-related-work-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if matches!(journey, Journey::EditorWorkspace) {
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "workspace",
        )
        .await?;
        let evidence =
            editor_workspace::verify(&access, &provider, root, node_did, session, &client_did)
                .await?;
        std::fs::write(
            root.join("editor-workspace-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if matches!(journey, Journey::EditorInventory) {
        editor_inventory::prepare(root).await?;
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        ensure!(
            client
                .call(id, "client_runtime_is_local", json!({"nodeDid":node_did}))
                .await
                == false,
            "inventory acceptance must use the enrolled runtime"
        );
        id += 1;
        let editor = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "review-inventory",
        )
        .await;
        if editor.is_err() {
            let observation = client
                .call(
                    id,
                    "desktop_session_file_inventory",
                    json!({
                        "nodeDid":node_did, "sessionId":session, "action":{"kind":"read"}
                    }),
                )
                .await;
            id += 1;
            let mut cursors = ["current", "previous"]
                .into_iter()
                .filter_map(|kind| {
                    let next = &observation[kind]["next"];
                    (!next.is_null()).then(|| next.clone())
                })
                .collect::<Vec<_>>();
            let mut observations = vec![observation];
            while let Some(cursor) = cursors.pop() {
                if observations.len() >= 4 {
                    break;
                }
                let page = client
                    .call(
                        id,
                        "desktop_session_file_inventory",
                        json!({
                            "nodeDid":node_did, "sessionId":session,
                            "action":{"kind":"read", "cursor":cursor}
                        }),
                    )
                    .await;
                id += 1;
                observations.push(page);
            }
            tokio::fs::write(
                root.join("editor-inventory-failure.json"),
                serde_json::to_vec_pretty(&observations)?,
            )
            .await?;
            #[cfg(feature = "editor-e2e")]
            copy_editor_evidence(root, "editor-inventory-failure.json")?;
        }
        editor?;
        let evidence = editor_inventory::verify(
            &mut client,
            &mut id,
            &access,
            root,
            node_did,
            session,
            &client_did,
            &identity,
        )
        .await?;
        tokio::fs::write(
            root.join("editor-inventory-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )
        .await?;
    }
    if matches!(
        journey,
        Journey::EditorReview | Journey::EditorReviewPermissions
    ) {
        editor_review::prepare(root)?;
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        ensure!(
            client
                .call(id, "client_runtime_is_local", json!({"nodeDid": node_did}))
                .await
                == false,
            "review must use the paired runtime without local operator access"
        );
        id += 1;
        let result = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            journey
                .editor_scenario()
                .context("recorded review journey")?,
        )
        .await;
        std::fs::write(
            root.join("editor-review-provider.json"),
            serde_json::to_vec_pretty(&provider.captured_chat_requests())?,
        )?;
        #[cfg(feature = "editor-e2e")]
        copy_editor_evidence(root, "editor-review-provider.json")?;
        result?;
        let evidence = editor_review::verify(
            &mut client,
            &mut id,
            &access,
            root,
            node_did,
            session,
            &client_did,
        )
        .await?;
        std::fs::write(
            root.join("editor-review-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        if matches!(journey, Journey::EditorReviewPermissions) {
            let permissions = editor_review::verify_permission_recovery(
                &mut client,
                &mut id,
                &access,
                root,
                &node_home,
                &client_did,
                &evidence,
                &identity,
            )
            .await?;
            std::fs::write(
                root.join("editor-review-permissions-canonical.json"),
                serde_json::to_vec_pretty(&permissions)?,
            )?;
        }
    }
    if matches!(journey, Journey::EditorGoalReplacement) {
        editor_goal_replacement::prepare(root)?;
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let releases: Vec<_> = (0..3)
            .map(|index| {
                let release = root.join(format!("finish-replaced-goal-{index}"));
                let held = Arc::clone(&goal_replacement);
                tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                    loop {
                        if release.is_file() {
                            held.release(index);
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }))
            })
            .collect();
        let editor_result = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "goal-replacement",
        )
        .await;
        std::fs::write(
            root.join("editor-goal-replacement-provider.json"),
            serde_json::to_vec_pretty(&provider.captured_chat_requests())?,
        )?;
        #[cfg(feature = "editor-e2e")]
        if root.join("editor-artifacts-path").is_file() {
            copy_editor_evidence(root, "editor-goal-replacement-provider.json")?;
        }
        editor_result?;
        let evidence = editor_goal_replacement::verify(
            &access,
            &provider,
            root,
            node_did,
            session,
            &client_did,
            &identity,
        )
        .await?;
        std::fs::write(
            root.join("editor-goal-replacement-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        drop(releases);
    }
    if matches!(journey, Journey::EditorGoalRemoval) {
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let releases: Vec<_> = (0..2)
            .map(|index| {
                let release = root.join(format!("finish-removed-goal-{index}"));
                let held = Arc::clone(&goal_removal);
                tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                    loop {
                        if release.is_file() {
                            held.release(index);
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }))
            })
            .collect();
        let editor_result = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "goal-removal",
        )
        .await;
        std::fs::write(
            root.join("editor-goal-removal-provider.json"),
            serde_json::to_vec_pretty(&provider.captured_chat_requests())?,
        )?;
        #[cfg(feature = "editor-e2e")]
        copy_editor_evidence(root, "editor-goal-removal-provider.json")?;
        editor_result?;
        let evidence =
            editor_goal_removal::verify(&access, &provider, root, node_did, &client_did).await?;
        std::fs::write(
            root.join("editor-goal-removal-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        drop(releases);
    }
    if matches!(journey, Journey::EditorGoalRecovery) {
        std::fs::write(
            root.join("editor-goal-recovery-fixture.json"),
            serde_json::to_vec(&json!({
                "draft":editor_goal_recovery::DRAFT, "captured":editor_goal_recovery::CAPTURED,
                "resuming":editor_goal_recovery::RESUMING, "wrapup":editor_goal_recovery::WRAPUP,
                "title":editor_goal_recovery::TITLE, "blockedReason":editor_goal_recovery::BLOCKED_REASON,
            }))?,
        )?;
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let release = root.join("finish-recovered-goal-provider");
        let held = Arc::clone(&goal_recovery);
        let _release = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                if release.is_file() {
                    held.release();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }));
        let editor_result = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "goals",
        )
        .await;
        std::fs::write(
            root.join("editor-goal-recovery-provider.json"),
            serde_json::to_vec_pretty(&provider.captured_chat_requests())?,
        )?;
        #[cfg(feature = "editor-e2e")]
        if root.join("editor-artifacts-path").is_file() {
            copy_editor_evidence(root, "editor-goal-recovery-provider.json")?;
        }
        editor_result?;
        let observation =
            wait_goal(&mut client, &mut id, node_did, session, "budget_limited").await?;
        let evidence = editor_goal_recovery::verify(
            &access,
            &provider,
            root,
            node_did,
            session,
            &client_did,
            observation,
        )
        .await?;
        std::fs::write(
            root.join("editor-goal-recovery-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if matches!(journey, Journey::EditorRetry) {
        std::fs::write(
            root.join("editor-retry-fixture.json"),
            serde_json::to_vec(&json!({
                "prompt":editor_retry::PROMPT,
                "reply":editor_retry::REPLY,
                "captured":editor_retry::CAPTURED,
            }))?,
        )?;
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let editor_result = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "retry",
        )
        .await;
        std::fs::write(
            root.join("editor-retry-provider.json"),
            serde_json::to_vec_pretty(&provider.captured_chat_requests())?,
        )?;
        #[cfg(feature = "editor-e2e")]
        copy_editor_evidence(root, "editor-retry-provider.json")?;
        editor_result?;
        let evidence = editor_retry::verify(
            &mut client,
            &mut id,
            &access,
            &provider,
            root,
            node_did,
            session,
            &client_did,
        )
        .await?;
        std::fs::write(
            root.join("editor-retry-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
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
    if matches!(journey, Journey::Conversation | Journey::Editor) {
        exercise_message_edit(
            &mut client,
            &mut id,
            node_did,
            session,
            agent_id,
            &paths,
            &access,
        )
        .await?;
    }
    client.close().await;
    stop(&mut host)
        .await
        .context("stopping the fixture client host")?;
    stop(&mut runtime)
        .await
        .context("stopping the fixture runtime")?;
    Ok(())
}

async fn exercise_message_edit(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    source: &str,
    agent: &str,
    paths: &DesktopPaths,
    access: &gents::ConfigAccess,
) -> Result<()> {
    let before = read_session(client, id, node_did, source).await?;
    let snapshot = client.call(*id, "desktop_client_snapshot", json!({})).await;
    *id += 1;
    let client_did = snapshot["client"]["clientDid"]
        .as_str()
        .context("missing native client identity")?;
    let child_session_id = format!("retained-edit-child-{id}");
    let message = before["timelineItems"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["kind"] == "userMessage"))
        .context("completed source has no authored user message")?;
    let padded = format!(
        " {} ",
        message["itemKey"]
            .as_str()
            .context("canonical message key")?
    );
    let invalid = client
        .try_call(
            *id,
            "desktop_session_fork_before_message",
            json!({"request": {
                "nodeDid": node_did, "sessionId": source, "messageKey": padded,
                "clientDid": client_did, "childSessionId": child_session_id
            }}),
        )
        .await;
    *id += 1;
    ensure!(
        invalid.is_err(),
        "message-key whitespace changed canonical identity"
    );
    let args = json!({"request": {
        "nodeDid": node_did, "sessionId": source, "messageKey": message["itemKey"],
        "clientDid": client_did, "childSessionId": child_session_id
    }});
    client.send(json!({"kind":"call", "id":*id, "command":{"name":"desktop_session_fork_before_message", "args":args}})).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = client.next().await;
            if response["kind"] == "response" && response["id"] == *id {
                ensure!(
                    response["error"].is_null(),
                    "fork failed before reply interception: {}",
                    response["error"]
                );
                drop(response);
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("intercepting the committed fork reply")??;
    *id += 1;
    let query = format!("{{ AgentSession(filter: {{ node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}} }}) {{ _docID session_id provenance }} AgentMessage(filter: {{ node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}} }}, order: {{sequence: ASC}}) {{_docID message_key sequence}} }}",
        gents::graphql::escape_graphql_string(node_did), gents::graphql::escape_graphql_string(&child_session_id),
        gents::graphql::escape_graphql_string(node_did), gents::graphql::escape_graphql_string(&child_session_id));
    let committed = access.execute(&query).await?;
    let previous = std::mem::replace(client, Attachment::start(paths).await);
    previous.close().await;
    let fork = client
        .call(*id, "desktop_session_fork_before_message", args)
        .await;
    *id += 1;
    ensure!(
        fork["sessionId"] == child_session_id,
        "fork recovery changed the retained destination"
    );
    ensure!(
        access.execute(&query).await? == committed,
        "fork recovery rewrote or duplicated the committed child"
    );
    let child = fork["sessionId"].as_str().context("missing fork receipt")?;
    ensure!(
        child != source && fork["nodeDid"] == node_did,
        "fork receipt crossed source scope"
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let view = client
                .call(
                    *id,
                    "desktop_session_goal",
                    json!({"nodeDid":node_did,"sessionId":child}),
                )
                .await;
            *id += 1;
            if view["sessionId"] == child {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("the fork's signed goal observation did not reach the client")?;
    let sent = client
        .call(
            *id,
            "desktop_chat_send",
            json!({"request": {
                "nodeDid": node_did, "sessionId": child, "agentId": agent,
                "content": "Return the edited fixture response."
            }}),
        )
        .await;
    *id += 1;
    ensure!(
        sent["sessionId"] == child,
        "edited input was not sent to the branch"
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let branch = read_session(client, id, node_did, child).await?;
            if branch["turnState"] == "completed" {
                let users = branch["timelineItems"]
                    .as_array()
                    .context("child timeline")?
                    .iter()
                    .filter(|item| item["kind"] == "userMessage")
                    .collect::<Vec<_>>();
                ensure!(
                    users.len() == 1
                        && users[0]["content"] == "Return the edited fixture response.",
                    "the edited branch retained the replaced user turn: {branch}"
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("edited branch did not complete")??;
    let after = read_session(client, id, node_did, source).await?;
    ensure!(
        after["timelineItems"] == before["timelineItems"],
        "editing mutated the original conversation"
    );
    Ok(())
}

async fn run_editor_journey(
    root: &Path,
    paths: &DesktopPaths,
    host_binary: &Path,
    session: &str,
    endpoint: &str,
    scenario: &str,
) -> Result<()> {
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
    let mut editor = Command::new("node");
    if scenario == "full" && std::env::var("GENTS_VSCODE_STARTUP_CUSTODY").as_deref() == Ok("file")
    {
        let startup_paths = DesktopPaths::from_root(root.join("startup-client"));
        let startup = Arc::new(
            ClientCore::start_with_paths_and_options(
                startup_paths.clone(),
                ClientCoreOptions::local_only(),
            )
            .await?,
        );
        startup.shutdown().await?;
        editor.env("GENTS_VSCODE_STARTUP_CLIENT_HOME", startup_paths.root());
    }
    let log = std::fs::File::create(root.join("editor.log"))?;
    let child = editor
        .env("GENTS_VSCODE_INFERENCE_ENDPOINT", endpoint)
        .env("GENTS_VSCODE_EDITOR_JOURNEY", scenario)
        .arg(script)
        .arg(root)
        .arg(paths.root())
        .arg(host_binary)
        .arg(session)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .status();
    let timeout = if scenario == "history" && editor_history::Options::from_env()?.profile {
        Duration::from_secs(600)
    } else {
        Duration::from_secs(120)
    };
    let status = tokio::time::timeout(timeout, child).await;
    let status = status.context("editor journey timed out")??;
    ensure!(status.success(), "editor journey exited with {status}");
    Ok(())
}

async fn verify_editor_steering(
    access: &gents::ConfigAccess,
    provider: &FakeLlm,
    root: &Path,
    node_did: &str,
    session: &str,
    original_doc_id: &str,
    client_did: &str,
) -> Result<Value> {
    use gents_protocol::request_input::QueueDelivery;
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }} }}) {{ {} superseded_by_request_doc_id }} }}"#,
        gents::graphql::escape_graphql_string(node_did), gents::graphql::escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    ensure!(
        rows.len() == 3,
        "steering must retain one active row and two signed versions of the input: {result}"
    );
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.admission_signer_did.as_deref() == Some(client_did)
                && row.requester_did.as_deref() == Some(client_did),
            "steering changed the enrolled author"
        );
    }
    let active = rows
        .iter()
        .find(|row| row.doc_id.as_deref() == Some(original_doc_id))
        .context("original request")?;
    let input = |delivery| {
        rows.iter().find(|row| {
            row.content.as_deref() == Some(STEERING_INPUT)
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| queue.delivery == delivery)
        })
    };
    let queued = input(QueueDelivery::Queue).context("original queued input")?;
    let steered = input(QueueDelivery::Steer).context("promoted input")?;
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-steering-ui.json"))?)?;
    let captured = &queued
        .input
        .as_ref()
        .context("queued input")?
        .captured_context;
    ensure!(
        captured.len() == 1
            && steered.input.as_ref().map(|input| &input.captured_context) == Some(captured),
        "Steer changed or dropped the queued capture"
    );
    let capture = serde_json::to_value(&captured[0])?;
    ensure!(
        capture["content"] == "queued captured unsaved context\n"
            && capture["uri"] == ui["contextUri"]
            && capture["kind"] == "selection"
            && capture["source"] == "editor"
            && capture["unsaved"] == true
            && capture["version"].as_u64().is_some_and(|v| v > 1)
            && capture["range"]["start"] == json!({"line":0,"character":0})
            && capture["range"]["end"] == json!({"line":1,"character":0}),
        "Steer did not retain the submitted editor observations"
    );
    ensure!(
        active.lifecycle_state == Some(RequestLifecycleState::Completed)
            && queued.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && steered.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && queued.superseded_by_request_doc_id == steered.doc_id
            && steered.superseded_by_request_doc_id.as_deref() == Some(original_doc_id),
        "the pending row must be promoted and consumed by the original physical request: {result}"
    );
    let rendered = access.execute(&format!(
        r#"{{ RenderedRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ request_doc_id turn_index }} }}"#,
        gents::graphql::escape_graphql_string(&active.request_id),
    )).await?;
    let rendered = rendered["data"]["RenderedRequest"]
        .as_array()
        .context("rendered steering requests")?;
    ensure!(
        rendered.len() == 2
            && rendered
                .iter()
                .all(|row| row["request_doc_id"] == original_doc_id)
            && rendered[0]["turn_index"] != rendered[1]["turn_index"],
        "steering must continue the original capture exactly once: {rendered:?}"
    );
    let requests = provider.captured_chat_requests();
    let provider_input_occurrences = requests
        .iter()
        .map(|body| body["messages"].to_string().matches(STEERING_INPUT).count())
        .sum::<usize>();
    let steered_rounds = requests
        .iter()
        .filter(|body| body["messages"].to_string().contains(STEERING_INPUT))
        .collect::<Vec<_>>();
    ensure!(
        steered_rounds.len() == 1
            && steered_rounds[0]["messages"]
                .to_string()
                .matches(STEERING_INPUT)
                .count()
                == 1,
        "the provider must receive the promoted input exactly once: {requests:?}"
    );
    let capture_count = |body: &Value| {
        body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .filter_map(|text| {
                text.strip_prefix(
                    "Editor context (captured text; URI identifies its editor location):\n",
                )
            })
            .filter_map(|text| serde_json::from_str::<Value>(text).ok())
            .filter(|value| value == &capture)
            .count()
    };
    let captured_parts = capture_count(steered_rounds[0]);
    let all_captured_parts = requests.iter().map(capture_count).sum::<usize>();
    ensure!(
        captured_parts == 1 && all_captured_parts == 1,
        "the provider lost, duplicated or received the capture before Steer"
    );
    Ok(json!({
        "nodeDid": node_did, "sessionId": session, "authorDid": client_did,
        "activeRequestDocId": original_doc_id, "queuedRequestDocId": queued.doc_id,
        "steeredRequestDocId": steered.doc_id, "activeState": active.lifecycle_state,
        "queuedState": queued.lifecycle_state, "steeredState": steered.lifecycle_state,
        "queuedReplacedBy": queued.superseded_by_request_doc_id,
        "steeredConsumedBy": steered.superseded_by_request_doc_id,
        "renderedRequests": rendered, "providerInputOccurrences": provider_input_occurrences,
        "providerRequests": requests.len(),
        "verifiedSignatures": rows.len(),
        "context": capture, "providerCaptureOccurrences": all_captured_parts,
    }))
}

async fn read_session(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    session: &str,
) -> Result<Value> {
    read_session_page(client, id, node_did, session, None).await
}

async fn read_session_page(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    session: &str,
    before: Option<&str>,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = client
                .try_call(
                    *id,
                    "desktop_session_snapshot",
                    json!({
                        "nodeDid":node_did, "sessionId":session, "timelineLimit":40,
                        "timelineBeforeItemKey":before,
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
    endpoint: &str,
) -> Result<()> {
    let local = client
        .call(*id, "client_runtime_is_local", json!({"nodeDid":node_did}))
        .await;
    *id += 1;
    ensure!(
        local == true,
        "the enrolled local runtime lost its local home observation"
    );
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
    let deployment = client
        .call(
            *id,
            "client_inference_setup_state",
            json!({"nodeDid":node_did}),
        )
        .await;
    *id += 1;
    ensure!(deployment["nodeDid"] == node_did);
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
    let observed = client
        .call(
            *id,
            "client_inference_setup_state",
            json!({"nodeDid":node_did}),
        )
        .await;
    *id += 1;
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    #[cfg(unix)]
    let status = tokio::time::timeout_at(deadline, child.wait())
        .await
        .context("fixture process did not exit after interrupt")??;
    #[cfg(windows)]
    let status = {
        let status = tokio::time::timeout_at(deadline, child.wait_for_exit())
            .await
            .context("fixture process did not exit after console interrupt")??;
        tokio::time::timeout_at(deadline, child.finish_output())
            .await
            .context("fixture process exited but console output cleanup did not finish")??;
        status
    };
    ensure!(status.success(), "fixture child exited with {status}");
    Ok(())
}
