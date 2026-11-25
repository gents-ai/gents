use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents_desktop_bridge::client_runtime::{ClientRuntime, ClientRuntimeOptions};
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::bounded_process::BoundedProcess;
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
#[path = "support/editor_command_output.rs"]
mod editor_command_output;
#[path = "support/editor_goal_delivery.rs"]
mod editor_goal_delivery;
#[path = "support/editor_goal_recovery.rs"]
mod editor_goal_recovery;
#[path = "support/editor_history.rs"]
mod editor_history;
#[path = "support/editor_images.rs"]
mod editor_images;
#[path = "support/editor_related_work.rs"]
mod editor_related_work;
#[path = "support/editor_retry.rs"]
mod editor_retry;
#[path = "support/editor_steering.rs"]
mod editor_steering;
#[path = "support/editor_workspace.rs"]
mod editor_workspace;
#[path = "support/goal_delivery.rs"]
mod goal_delivery;
#[path = "support/live_runtime.rs"]
mod live_runtime;
#[path = "support/passive_settings.rs"]
mod passive_settings;
#[path = "support/runtime_replacement.rs"]
mod runtime_replacement;
use crate::support;

use gents::support::ports;
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamResponse, StreamScript,
};
use support::Attachment;

#[derive(Clone, Copy)]
enum Journey {
    PassiveSettings {
        installed: bool,
    },
    /// Restarts the client during reply recovery. Transient deltas need not
    /// survive that outage, so hold the provider response through Goal controls
    /// and qualify the same owned request plus its final durable output.
    Goal,
    /// One of the installed editor's acceptance scenarios (`scenario`).
    #[cfg(feature = "editor-e2e")]
    Editor(&'static str),
}

/// The editor acceptance scenarios this driver serves a provider for.
mod scenario {
    pub const STEERING: &str = "steering";
    pub const GOAL_STEERING: &str = "goal-steering";
    pub const HISTORY: &str = "history";
    pub const GOALS: &str = "goals";
    pub const GOAL_DELIVERY: &str = "goal-delivery";
    pub const IMAGES: &str = "images";
    pub const RELATED_WORK_FAILURE: &str = "related-work-failure";
}

impl Journey {
    fn editor_scenario(self) -> Option<&'static str> {
        match self {
            #[cfg(feature = "editor-e2e")]
            Self::Editor(scenario) => Some(scenario),
            _ => None,
        }
    }

    fn is_editor(self, scenario: &str) -> bool {
        self.editor_scenario() == Some(scenario)
    }

    fn is_steering(self) -> bool {
        self.is_editor(scenario::STEERING) || self.is_editor(scenario::GOAL_STEERING)
    }
}

const STEERING_INPUT: &str = "Steering input from the installed editor.";
const STEERING_REPLY: &str = "Steering input received.";

#[cfg(feature = "editor-e2e")]
#[cfg_attr(
    windows,
    ignore = "installed reply-loss relay requires a POSIX executable fixture"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_recovers_goal_commands_after_lost_replies_and_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Editor(scenario::GOAL_DELIVERY))
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-goal-delivery-canonical.json")
}

#[cfg(all(feature = "editor-e2e", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_recovers_text_goal_commands_after_lost_replies_and_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-goal-delivery")
        .await
        .with_context(|| {
            std::fs::read_to_string(temp.path().join("editor.log")).unwrap_or_default()
        })?;
    copy_editor_evidence(temp.path(), "editor-goal-delivery-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[cfg_attr(
    windows,
    ignore = "installed reply-loss relay requires a POSIX executable fixture"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_recovers_the_original_message_after_a_lost_helper_reply() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-delivery")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-delivery-canonical.json")?;
    copy_editor_evidence(temp.path(), "editor-fork-canonical.json")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives a local model; run through `make live-client`"]
async fn native_client_submits_signed_input_and_observes_live_and_durable_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = live_runtime::exercise(temp.path(), false).await;
    result.with_context(|| support::fixture_logs(temp.path(), &[]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives a local model; run through `make live-client`"]
async fn managed_runtime_replacement_preserves_running_input_enrollment_and_transcript(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_replacement(temp.path())
        .await
        .with_context(|| support::fixture_logs(temp.path(), &["runtime-replacement.err"]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_client_resumes_a_durable_goal_and_observes_completion() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Goal)
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))
}

#[cfg(all(feature = "editor-e2e", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_steers_text_input_into_a_live_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-steering").await?;
    copy_editor_evidence(temp.path(), "editor-steering-canonical.json")
}

#[cfg(all(feature = "editor-e2e", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_steers_text_input_and_preserves_a_live_goal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-goal-steering").await?;
    copy_editor_evidence(temp.path(), "editor-steering-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_preserves_live_session_and_draft_across_window_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_steers_pending_input_into_the_active_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Editor(scenario::STEERING))
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-steering-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_steers_pending_input_and_continues_the_same_goal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Editor(scenario::GOAL_STEERING))
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-steering-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_pages_native_history_without_losing_the_reader_or_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let evidence = editor_history::ProfileEvidence::from_env(temp.path())?;
    let result = exercise(temp.path(), Journey::Editor(scenario::HISTORY))
        .await
        .and_then(|()| copy_editor_evidence(temp.path(), "editor-history-canonical.json"));
    if let Some(evidence) = evidence {
        evidence.finish(temp.path(), &result)?;
    }
    result.with_context(|| {
        support::fixture_logs(
            temp.path(),
            &[
                "editor-history-seed-progress.json",
                "editor-history-seed-failure.json",
            ],
        )
    })
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_preserves_captured_context_in_a_multi_root_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-workspace")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-workspace-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_preserves_image_bytes_through_submission_and_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = exercise(temp.path(), Journey::Editor(scenario::IMAGES)).await;
    result.with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-images-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_retries_failed_input_without_submitting_the_unsent_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "retry")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-retry-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_reviews_retained_file_images_without_reading_the_working_copy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-review")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-review-canonical.json")
}

#[cfg(all(feature = "editor-e2e", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_restores_and_saves_recorded_edits_without_overwriting_later_changes() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-undo")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-undo-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_pages_real_file_captures_across_reload_and_new_activity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    if let Err(error) = live_runtime::exercise_editor(temp.path(), "live-review-inventory").await {
        return Err(error.context(support::fixture_logs(temp.path(), &[])));
    }
    let root = temp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        copy_editor_evidence(&root, "editor-inventory-canonical.json")
    })
    .await?
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_recovers_blocked_and_limited_goals_without_submitting_the_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Editor(scenario::GOALS))
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-goal-recovery-canonical.json")
}

/// Releases the reply every request not answered otherwise waits on.
const PROVIDER_GATE: &str = "provider";
/// Releases the Goal continuation after a steered reply.
const CONTINUATION_GATE: &str = "goal-steering-continuation";
/// Releases the profiled history journey's held reply.
const HISTORY_GATE: &str = "history";
/// Never released: holds the reply through the Goal journey's controls.
const GOAL_GATE: &str = "goal";

/// The scenario providers and hold files the editor journeys' provider reads.
struct ProviderReplies<'a> {
    goal_delivery: &'a editor_goal_delivery::Provider,
    related: &'a editor_related_work::Provider,
    goal_recovery: &'a editor_goal_recovery::Provider,
    history_hold: &'a Path,
    goal_hold: &'a Path,
}

/// The scenario's reply to `request` when it answers it, else a reply held
/// at its gate.
fn provider_reply(
    request: &Value,
    journey: Journey,
    history: Option<editor_history::Options>,
    activity_profile: bool,
    replies: &ProviderReplies<'_>,
) -> StreamResponse {
    if journey.is_editor(scenario::IMAGES) {
        if let Some(reply) = editor_images::reply(request) {
            return reply;
        }
    }
    if journey.is_editor(scenario::GOAL_DELIVERY) {
        if let Some(reply) = replies.goal_delivery.reply(request) {
            return reply;
        }
    }
    if journey.is_editor(scenario::RELATED_WORK_FAILURE) {
        if let Some(reply) = replies.related.reply(request) {
            return reply;
        }
    }
    if journey.is_editor(scenario::GOALS) {
        if let Some(reply) = replies.goal_recovery.reply(request) {
            return reply;
        }
    }
    if activity_profile && journey.is_editor(scenario::HISTORY) {
        if let Some(reply) = editor_command_output::reply(request) {
            return reply;
        }
    }
    if let Some(options) = history {
        if let Some(reply) = editor_history::reply(request, options) {
            return reply;
        }
    }
    if journey.is_editor(scenario::GOAL_STEERING) && editor_steering::has_completed_reply(request) {
        return StreamResponse::Stream(
            StreamScript::paused(CONTINUATION_GATE, ["Goal continuation after steering"])
                .then(vec![StreamChunk::text(" and finished.")]),
        );
    }
    if journey.is_steering() && request["messages"].to_string().contains(STEERING_INPUT) {
        return StreamResponse::text(STEERING_REPLY);
    }
    if matches!(journey, Journey::Goal) {
        return StreamResponse::text("First part and final part.").after(PROVIDER_GATE);
    }
    let gate = if replies.history_hold.is_file() {
        HISTORY_GATE
    } else if replies.goal_hold.is_file() {
        GOAL_GATE
    } else {
        PROVIDER_GATE
    };
    StreamResponse::Stream(
        StreamScript::paused(gate, ["First part"])
            .then(vec![StreamChunk::text(" and final part.")]),
    )
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
    live_runtime::exercise_editor(temp.path(), "live-goal-removal")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-goal-removal-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_replaces_goals_in_the_same_chat_without_losing_work_or_drafts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-goal-replacement")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-goal-replacement-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_answers_a_native_question_and_uses_the_edited_worker_prompt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-questions")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-questions-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_observes_concurrent_related_work_without_losing_the_parent_draft() -> Result<()> {
    let temp = tempfile::tempdir()?;
    live_runtime::exercise_editor(temp.path(), "live-related-work")
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-related-work-canonical.json")
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_observes_related_worker_failure_without_losing_completed_work_or_the_parent_draft(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Editor(scenario::RELATED_WORK_FAILURE))
        .await
        .with_context(|| support::fixture_logs(temp.path(), &[]))?;
    copy_editor_evidence(temp.path(), "editor-related-work-canonical.json")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_model_settings_refresh_without_the_runtime_key_file() -> Result<()> {
    let root = tempfile::tempdir()?;
    exercise(root.path(), Journey::PassiveSettings { installed: false }).await
}

#[cfg(all(target_os = "macos", feature = "editor-e2e"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installed_editor_refreshes_models_and_reconnects_without_the_runtime_key_file(
) -> Result<()> {
    let root = tempfile::tempdir()?;
    match exercise(root.path(), Journey::PassiveSettings { installed: true }).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let retained = root.keep();
            Err(error).with_context(|| {
                format!(
                    "disposable settings fixture retained at {}",
                    retained.display()
                )
            })
        }
    }
}

async fn exercise(root: &Path, journey: Journey) -> Result<()> {
    #[cfg(feature = "editor-e2e")]
    if journey.is_editor(scenario::HISTORY)
        && !editor_history::Options::from_env()?.profile
        && std::env::var_os("GENTS_PREVIOUS_CLIENT_HOST").is_none()
    {
        return live_runtime::exercise_editor(root, "live-history").await;
    }
    let history = journey
        .is_editor(scenario::HISTORY)
        .then(editor_history::Options::from_env)
        .transpose()?;
    let current_cli = support::native_binary("gents")?;
    let current_host_binary = support::native_binary("gents-client-host")?;
    let host_binary = current_host_binary;
    let cli = current_cli;
    if journey.editor_scenario().is_some() {
        std::fs::write(
            root.join("AGENTS.md"),
            "# Project instructions fixture\nUse the current editor workspace.\n",
        )?;
    }
    let goal_provider_hold = root.join("hold-goal-provider");
    let provider_goal_recovery = editor_goal_recovery::Provider::default();
    let provider_goal_delivery = editor_goal_delivery::Provider::default();
    let history_provider_hold = root.join("hold-history-provider");
    let activity_profile = std::env::var("GENTS_VSCODE_ACTIVITY_PROFILE").as_deref() == Ok("1");
    ensure!(
        !activity_profile || cfg!(unix),
        "The command-output activity fixture requires a Unix shell."
    );
    let related_provider = Arc::new(editor_related_work::Provider::new(
        journey.is_editor(scenario::RELATED_WORK_FAILURE),
        activity_profile,
    ));
    let provider_related = Arc::clone(&related_provider);
    let completion_notify = Arc::new(tokio::sync::Notify::new());
    let notify_completions = Arc::clone(&completion_notify);
    let provider = Arc::new(
        ScriptedBackend::builder("editor-fixture")
            .models(if journey.editor_scenario().is_some() {
                &["editor-fixture-alternate"][..]
            } else {
                &[]
            })
            .respond(move |request| {
                let reply = provider_reply(
                    request,
                    journey,
                    history,
                    activity_profile,
                    &ProviderReplies {
                        goal_delivery: &provider_goal_delivery,
                        related: &provider_related,
                        goal_recovery: &provider_goal_recovery,
                        history_hold: &history_provider_hold,
                        goal_hold: &goal_provider_hold,
                    },
                );
                notify_completions.notify_waiters();
                Some(reply)
            })
            .start()?,
    );
    let node_home = root.join("runtime");
    let homes = support::homes::ProcessHomes::under(root);
    let mut init = Command::new(&cli);
    init.current_dir(root)
        .envs(homes.env())
        .env("RUST_LOG", "warn,gents_server::commands::init=info")
        .args(["init", "--store-key-custody", "file", "--home"])
        .arg(&node_home)
        .arg("--desktop-home")
        .arg(homes.desktop_home())
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
    let capture = root.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        BoundedProcess::spawn(
            init.as_std_mut(),
            &capture,
            "init",
            "fixture initialization",
        )?
        .wait(Duration::from_secs(120))
    })
    .await??;
    let init_stages = std::fs::read_to_string(root.join("init.stderr"))?;
    ensure!(
        init_stages.contains(r#""embedded_node_drained""#),
        "initialization must record its stage boundaries so a timed-out run can be attributed: {init_stages}"
    );
    let init: Value = serde_json::from_slice(&output)?;
    let node_did = init["node_did"].as_str().context("initialized node DID")?;
    let identity = gents_store::identity::KeyIdentity::load_or_create(
        init["key_path"]
            .as_str()
            .context("initialized identity path")?,
        None,
    )?;
    let port = ports::allocate_port()?;
    let runtime_command = runtime_server_command(&cli, root, &node_home, port);
    #[cfg(unix)]
    let (mut runtime, ready, _runtime_drain) = {
        let mut runtime_command = runtime_command;
        let mut runtime = runtime_command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(
                root.join("runtime.err"),
            )?))
            .kill_on_drop(true)
            .spawn()?;
        let (ready, drain) = wait_ready(&mut runtime, &root.join("runtime.log")).await?;
        (runtime, ready, drain)
    };
    #[cfg(windows)]
    let mut runtime =
        console::ConsoleChild::spawn(runtime_command.as_std(), &root.join("runtime.log"))?;
    #[cfg(unix)]
    let status = read_status(&cli, &node_home, port).await?;
    #[cfg(windows)]
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
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await?,
    );
    let client_did = seed.node_identity().did().to_owned();
    seed.shutdown().await?;
    drop(seed);
    let mut host_command = Command::new(&host_binary);
    host_command
        .envs(homes.env())
        .args(["--serve", "--home"])
        .arg(paths.root());
    if journey.editor_scenario().is_some() || matches!(journey, Journey::PassiveSettings { .. }) {
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
    let mut client = Attachment::start_with_binary(&paths, &host_binary).await;
    // The operator observes and configures as the User `init` made the Node's
    // administrator, whose `session_read` covers the client's sessions and
    // requests; the Node key holds no relation on them.
    let operator =
        gents_store::client_journal::principal_identity::PrincipalIdentity::load_existing(
            &homes.cli_identity(),
        )?;
    let access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(graphql, operator.did())
            .with_client_signing_capability(operator.client_signing_capability()?)?,
    );
    let pending = client
        .call(
            1,
            "desktop_runtime_access_request",
            json!({
                "request": { "serverAddress": format!("http://127.0.0.1:{port}") }
            }),
        )
        .await;
    let request_id = pending["requestId"]
        .as_str()
        .context("pending client access")?;
    let gents_store::config_client::ConfigAccess::Graphql(endpoint) = &access else {
        anyhow::bail!("the fixture requires its User-signed HTTP endpoint");
    };
    let receipt = endpoint
        .decide_runtime_access(&gents_protocol::client_access::DecideClientAccess {
            node_did: node_did.to_owned(),
            request_id: request_id.to_owned(),
            decision: gents_protocol::client_access::ClientAccessDecision::Approve,
        })
        .await?;
    ensure!(receipt.state == gents_protocol::client_access::ClientAccessState::Approved);
    let mut id = 2;
    let deployment = wait_chat_deployment(&mut client, &mut id, node_did, &access).await?;
    if matches!(journey, Journey::PassiveSettings { .. }) {
        client = passive_settings::exercise(
            client,
            &mut id,
            &paths,
            &host_binary,
            Path::new(init["key_path"].as_str().context("runtime key path")?),
            node_did,
            matches!(journey, Journey::PassiveSettings { installed: true }),
        )
        .await?;
        client.close().await;
        stop(&mut runtime)
            .await
            .context("stopping the metadata fixture runtime")?;
        stop(&mut host)
            .await
            .context("stopping the metadata fixture client")?;
        return Ok(());
    }

    let agent_did = deployment["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["isDefault"] == true))
        .and_then(|agent| agent["agentDid"].as_str())
        .context("runtime default agent")?;
    if journey.is_editor(scenario::GOAL_DELIVERY) {
        client
            .call(id, "client_enroll_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        wait_chat_deployment(&mut client, &mut id, node_did, &access).await?;
        configure_inference(
            &mut client,
            &mut id,
            node_did,
            agent_did,
            provider.endpoint(),
        )
        .await?;
    }
    let content = "Return the fixture response.".to_owned();
    let receipt = if matches!(journey, Journey::Goal) {
        let mut recovery = EditRecoveryHost {
            process: &mut host,
            binary: &host_binary,
            root,
            paths: &paths,
            access: &access,
            editor_store: false,
        };
        let (reconnected, result) = goal_delivery::exercise(
            client,
            &mut id,
            node_did,
            &client_did,
            agent_did,
            &identity,
            &mut recovery,
        )
        .await?;
        client = reconnected;
        result
    } else if journey.is_editor(scenario::GOALS) || journey.is_editor(scenario::GOAL_STEERING) {
        let request = json!({
            "nodeDid":node_did, "agentDid":agent_did,
            "cwd": journey.is_steering().then_some(root),
            "action":{"kind":"create", "objective":"Return the fixture response.", "tokenBudget":"1000"}
        });
        client
            .call(id, "desktop_goal_control", json!({"request":request}))
            .await
    } else {
        client
            .call(
                id,
                "desktop_chat_send",
                json!({"request":{
                    "nodeDid":node_did,"agentDid":agent_did,"content":content,"delivery":"queue",
                    "cwd": journey.is_steering().then_some(root)
                }}),
            )
            .await
    };
    id += 1;
    let session = receipt["sessionId"].as_str().context("submitted session")?;
    let request = receipt["requestId"].as_str().context("submitted request")?;
    let signed_query = format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
        gents_store::graphql::escape_graphql_string(request),
        gents::SIGNED_REQUEST_FIELDS,
    );
    let rows = tokio::time::timeout(
        Duration::from_secs(30),
        gents_store::test_support::rows_when(&access, "AgentRequest", &signed_query, |data| {
            data["AgentRequest"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        }),
    )
    .await
    .context("submitted request did not reach the runtime")?;
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(rows["AgentRequest"][0].clone())?;
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
    let original_doc_id = row.doc_id.clone().context("submitted physical request")?;
    ensure!(row.admission_signer_did.as_deref() == Some(client_did.as_str()));
    ensure!(client_did != node_did);
    if matches!(journey, Journey::Goal) {
        let context = &row
            .input
            .as_ref()
            .context("signed captured input")?
            .captured_context;
        ensure!(
            context.len() == 1
                && context[0].unsaved
                && context[0].content == goal_delivery::CAPTURED_CONTENT
        );
    }
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
                return Ok::<_, anyhow::Error>(());
            }
            client.wait_for_update().await?;
        }
    })
    .await
    .context("the canonical session header was not observed")??;
    if !matches!(journey, Journey::Goal) {
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
                client.wait_for_update().await?;
            }
        })
        .await
        .context("live output was not observed before provider completion")??;
    }
    if journey.is_steering() {
        let completion_path = root.join("finish-provider");
        let backend = Arc::clone(&provider);
        let continuation_path = root.join("finish-goal-steer-provider");
        let _completion = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            if support::wait_for_file(&completion_path).await.is_ok() {
                backend.release(PROVIDER_GATE);
            }
            if journey.is_editor(scenario::GOAL_STEERING)
                && support::wait_for_file(&continuation_path).await.is_ok()
            {
                backend.release(CONTINUATION_GATE);
            }
        }));
        configured_actions::seed_editor_settings(&access, node_did, agent_did).await?;
        client
            .call(id, "client_enroll_local", json!({"nodeHome":node_home}))
            .await;
        let editor = run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            journey.editor_scenario().context("steering scenario")?,
        )
        .await;
        if editor.is_err() {
            let snapshot = read_session(&mut client, &mut id, node_did, session).await?;
            let requests = access.execute(&format!(
                r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} superseded_by_request_doc_id }} }}"#,
                gents_store::graphql::escape_graphql_string(node_did),
                gents_store::graphql::escape_graphql_string(session),
                gents::SIGNED_REQUEST_FIELDS,
            )).await?;
            let name = "editor-steering-failure.json";
            std::fs::write(
                root.join(name),
                serde_json::to_vec_pretty(&json!({
                    "snapshot": snapshot, "requests": requests,
                }))?,
            )?;
            #[cfg(feature = "editor-e2e")]
            copy_editor_evidence(root, name)?;
        }
        editor?;
        id += 1;
    }
    if journey.is_editor(scenario::GOALS) {
        wait_goal(&mut client, &mut id, node_did, session, "active").await?;
    }
    if matches!(journey, Journey::Goal) {
        let mut view = wait_goal(&mut client, &mut id, node_did, session, "active").await?;
        ensure!(view["goal"]["goalId"] == receipt["goalId"]);
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let notified = completion_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let captured = provider.observed_completion_bodies();
                if captured.iter().any(|request| {
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
                }) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .context("the held Goal provider request was not observed")?;
        let current = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id lifecycle_state }} }}"#,
                gents_store::graphql::escape_graphql_string(&original_doc_id)
            ))
            .await?;
        let rows = current["data"]["AgentRequest"]
            .as_array()
            .context("current Goal request rows")?;
        ensure!(
            rows.len() == 1
                && rows[0]["_docID"] == original_doc_id
                && rows[0]["request_id"] == request
                && rows[0]["lifecycle_state"] == "processing",
            "Goal recovery must preserve the same processing request before controls: {rows:?}"
        );
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
                session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}} }}) {{ _docID request_id lifecycle_state }} }}"#,
                gents_store::graphql::escape_graphql_string(node_did), gents_store::graphql::escape_graphql_string(session))).await?;
            let rows = requests["data"]["AgentRequest"]
                .as_array()
                .context("goal request rows")?;
            ensure!(
                rows.len() == 1
                    && rows[0]["_docID"] == original_doc_id
                    && rows[0]["request_id"] == request
                    && rows[0]["lifecycle_state"] == "processing",
                "goal controls must preserve the current owned turn"
            );
        }
        // The snapshot carries the Goal state `desktop_session_goal` reads
        // (Lean `SessionObservation.State`), so it shows a Goal only as the
        // Agent's verified observation shows it; that no Goal document
        // reaches the client is checked on its own store after the host stops.
        let projection = read_session(&mut client, &mut id, node_did, session).await?;
        let shown = &projection["goal"]["observation"];
        ensure!(
            projection["sessionId"] == session
                && projection["nodeDid"] == node_did
                && projection["goal"]["kind"] == "observed"
                && projection["goal"]["next"] == "control_goal"
                && shown["nodeDid"] == node_did
                && shown["sessionId"] == session
                && shown["goal"]["goalId"] == view["goal"]["goalId"]
                && shown["goal"]["status"] == view["goal"]["status"]
                && shown["goal"]["objective"] == view["goal"]["objective"],
            "the session snapshot must show the Agent's verified Goal observation: {} vs {}",
            projection["goal"],
            view["goal"]
        );
    }
    provider.release(PROVIDER_GATE);
    let expected_reply = if journey.is_steering() {
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
            client.wait_for_update().await?;
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
                Some("loading") => client.wait_for_update().await?,
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
    if matches!(journey, Journey::Goal) {
        wait_goal(&mut client, &mut id, node_did, session, "paused").await?;
        let requests = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ node_did: {{_eq: "{}"}},
            session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}} }}) {{ request_id }} }}"#,
                gents_store::graphql::escape_graphql_string(node_did),
                gents_store::graphql::escape_graphql_string(session)
            ))
            .await?;
        ensure!(
            requests["data"]["AgentRequest"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
            "the paused goal must finish its current turn without a continuation"
        );
    }
    if journey.is_editor(scenario::GOALS) {
        let blocked =
            editor_goal_recovery::wait_for_source_blocked(&access, node_did, session).await?;
        wait_goal(&mut client, &mut id, node_did, session, "blocked").await?;
        std::fs::write(
            root.join("editor-goal-blocked-source.json"),
            serde_json::to_vec_pretty(&blocked)?,
        )?;
    }
    let requests = provider.observed_completion_bodies();
    if matches!(journey, Journey::Goal) {
        let expected = format!("data:image/png;base64,{}", goal_delivery::IMAGE_DATA);
        let images: Vec<_> = requests
            .iter()
            .filter_map(|request| request["messages"].as_array())
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter(|part| part["type"] == "image_url")
            .collect();
        ensure!(
            images.len() == 1 && images[0]["image_url"]["url"] == expected,
            "the recovered Goal must deliver its frozen image to the provider exactly once: {images:?}"
        );
    }
    if journey.is_steering() {
        let evidence = editor_steering::verify(
            &access,
            &provider,
            editor_steering::Context {
                root,
                node_did,
                session,
                original_doc_id: &original_doc_id,
                client_did: &client_did,
                goal_id: journey
                    .is_editor(scenario::GOAL_STEERING)
                    .then(|| receipt["goalId"].as_str())
                    .flatten(),
            },
        )
        .await?;
        std::fs::write(
            root.join("editor-steering-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if journey.is_editor(scenario::HISTORY) {
        let options = history.context("history options")?;
        let mut evidence = editor_history::populate(
            editor_history::EditorRun {
                client: &mut client,
                id: &mut id,
                access: &access,
                node: node_did,
                session,
                author: &client_did,
            },
            agent_did,
            options,
            root,
        )
        .await?;
        evidence["commandOutputProfile"] = json!(activity_profile && options.profile);
        std::fs::write(
            root.join("editor-history-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        configured_actions::seed_editor_settings(&access, node_did, agent_did).await?;
        client
            .call(id, "client_enroll_local", json!({"nodeHome":node_home}))
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
        let backend = Arc::clone(&provider);
        let _completion = options.profile.then(|| {
            tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                if support::wait_for_file(&completion_path).await.is_ok() {
                    backend.release(HISTORY_GATE);
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
                editor_history::EditorRun {
                    client: &mut client,
                    id: &mut id,
                    access: &access,
                    node: node_did,
                    session,
                    author: &client_did,
                },
                options,
                activity_profile,
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
    if journey.is_editor(scenario::RELATED_WORK_FAILURE) {
        editor_related_work::configure(&access, node_did, agent_did).await?;
        client
            .call(id, "client_enroll_local", json!({"nodeHome":node_home}))
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
            related_provider.release_when_requested(root, &provider),
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
            journey.is_editor(scenario::RELATED_WORK_FAILURE),
            activity_profile,
        )
        .await?;
        std::fs::write(
            root.join("editor-related-work-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if journey.is_editor(scenario::IMAGES) {
        configured_actions::seed_editor_settings(&access, node_did, agent_did).await?;
        client
            .call(id, "client_enroll_local", json!({"nodeHome":node_home}))
            .await;
        run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "images",
        )
        .await?;
        let evidence =
            editor_images::verify(&access, &provider, root, node_did, session, &client_did).await?;
        std::fs::write(
            root.join("editor-images-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
    }
    if journey.is_editor(scenario::GOALS) {
        std::fs::write(
            root.join("editor-goal-recovery-fixture.json"),
            serde_json::to_vec(&json!({
                "draft":editor_goal_recovery::DRAFT, "captured":editor_goal_recovery::CAPTURED,
                "resuming":editor_goal_recovery::RESUMING, "wrapup":editor_goal_recovery::WRAPUP,
                "title":editor_goal_recovery::TITLE, "blockedReason":editor_goal_recovery::BLOCKED_REASON,
            }))?,
        )?;
        configured_actions::seed_editor_settings(&access, node_did, agent_did).await?;
        client
            .call(id, "client_enroll_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let release = root.join("finish-recovered-goal-provider");
        let held = Arc::clone(&provider);
        let _release = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            if support::wait_for_file(&release).await.is_ok() {
                held.release(editor_goal_recovery::GATE);
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
            serde_json::to_vec_pretty(&provider.observed_completion_bodies())?,
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
    if journey.is_editor(scenario::GOAL_DELIVERY) {
        editor_goal_delivery::prepare(root)?;
        let finish = root.join("finish-delivery-goal");
        let held = Arc::clone(&provider);
        let release = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            if support::wait_for_file(&finish).await.is_ok() {
                held.release(editor_goal_delivery::GATE);
            }
        }));
        run_editor_journey(
            root,
            &paths,
            &host_binary,
            session,
            provider.endpoint(),
            "goal-delivery",
        )
        .await?;
        editor_goal_delivery::verify_images(&provider)?;
        let evidence = editor_goal_delivery::verify(
            &access,
            &mut client,
            &mut id,
            node_did,
            &client_did,
            &identity,
            root,
        )
        .await?;
        std::fs::write(
            root.join("editor-goal-delivery-canonical.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        drop(release);
    }
    client.close().await;
    stop(&mut runtime)
        .await
        .context("stopping the fixture runtime")?;
    stop(&mut host)
        .await
        .context("stopping the fixture client host")?;
    if matches!(journey, Journey::Goal) {
        // Ordinary client routes replicate no application document (Lean
        // `ScopeTemplates.builtinCatalog`, `Identity.unlisted_collection_stays_local`):
        // the client's own store holds the Goal caller receipts it wrote, and
        // no Goal.
        let reopened = Arc::new(
            ClientRuntime::start_with_paths_and_options(
                paths.clone(),
                ClientRuntimeOptions {
                    store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                    ..Default::default()
                },
            )
            .await?,
        );
        let local = gents_store::config_client::ConfigAccess::local(reopened.node_arc())
            .execute("{ PendingChatSubmission { _docID } }")
            .await;
        reopened.shutdown().await?;
        let local = local?;
        ensure!(
            local["data"]["PendingChatSubmission"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "ordinary client routes must not replicate raw Goal documents: {local}"
        );
    }
    Ok(())
}

async fn wait_chat_deployment(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    access: &gents_store::config_client::ConfigAccess,
) -> Result<Value> {
    let mut last_snapshot = Value::Null;
    let observed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = client.snapshot(*id).await;
            *id += 1;
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
                return Ok::<_, anyhow::Error>(deployment.clone());
            }
            client.wait_for_update().await?;
        }
    })
    .await;
    match observed {
        Ok(result) => result,
        Err(error) => {
            let query = format!(
                "{{ NodeReadiness(filter: {{node_did: {{_eq: \"{}\"}}}}, limit: 2) {{_docID node_did snapshot_json updated_at}} }}",
                gents_store::graphql::escape_graphql_string(node_did),
            );
            let source = tokio::time::timeout(Duration::from_secs(2), access.execute(&query)).await;
            Err(error).with_context(|| {
                format!(
                    "client did not observe its authorized runtime route; runtime readiness: {source:?}; client snapshot: {last_snapshot}"
                )
            })
        }
    }
}

struct EditRecoveryHost<'a> {
    process: &'a mut Child,
    binary: &'a Path,
    root: &'a Path,
    paths: &'a DesktopPaths,
    access: &'a gents_store::config_client::ConfigAccess,
    editor_store: bool,
}

impl EditRecoveryHost<'_> {
    async fn restart(&mut self) -> Result<()> {
        stop(self.process)
            .await
            .context("stopping the retained-history helper")?;
        let mut command = Command::new(self.binary);
        command
            .envs(support::homes::ProcessHomes::under(self.root).env())
            .args(["--serve", "--home"])
            .arg(self.paths.root());
        if self.editor_store {
            command.arg("--editor-store");
        }
        #[cfg(unix)]
        let restarted = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.root.join("client-host-restart.log"))?,
            ))
            .kill_on_drop(true)
            .spawn()?;
        #[cfg(windows)]
        let restarted = console::ConsoleChild::spawn(
            command.as_std(),
            &self.root.join("client-host-restart.log"),
        )?;
        *self.process = restarted;
        wait_listener(self.process, self.paths).await
    }
}

async fn discard_successful_reply(
    client: &mut Attachment,
    id: u32,
    name: &str,
    args: Value,
) -> Result<()> {
    client
        .send(json!({"kind":"call", "id":id, "command":{"name":name, "args":args}}))
        .await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = client.next().await;
            if response["kind"] == "response" && response["id"] == id {
                ensure!(
                    response["error"].is_null(),
                    "{name} failed before reply interception: {}",
                    response["error"]
                );
                return Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await
    .context("intercepting the committed reply")?
}

fn runtime_server_command(binary: &Path, root: &Path, home: &Path, port: u16) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .envs(support::homes::ProcessHomes::under(root).env())
        .args(["server", "--home"])
        .arg(home)
        .arg("--http-port")
        .arg(port.to_string());
    command
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
    editor.envs(support::homes::ProcessHomes::under(root).env());
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
                    json!({"nodeDid":node_did, "sessionId":session, "timelineLimit":40}),
                )
                .await;
            *id += 1;
            match result {
                Ok(projection) => return Ok(projection),
                Err(error)
                    if error.code
                        == gents_store::client_error::BridgeErrorCode::ProjectionPending =>
                {
                    client.wait_for_update().await?;
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
    let mut last = Value::Null;
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
            let observation = &view["observation"];
            if view["kind"] == "observed"
                && observation["nodeDid"] == node_did
                && observation["sessionId"] == session
                && observation["goal"]["status"] == status
            {
                return Ok::<_, anyhow::Error>(observation.clone());
            }
            last = view;
            client.wait_for_update().await?;
        }
    })
    .await
    .with_context(|| {
        format!("the signed goal observation did not reach the client; last: {last}")
    })?
}

async fn configure_inference(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    agent_did: &str,
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
        .and_then(|rows| rows.iter().find(|row| row["agent_did"] == agent_did))
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
        observed["agentConfigs"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["agent_did"] == agent_did
                && row["inference_profile_id"] == "editor-setup")),
        "the selected model was not bound to the default agent: {observed}"
    );
    Ok(())
}

#[cfg(unix)]
async fn wait_ready(
    child: &mut Child,
    path: &Path,
) -> Result<(Value, tokio_util::task::AbortOnDropHandle<Result<()>>)> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    let stdout = child.stdout.take().context("runtime stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut log = tokio::fs::File::create(path).await?;
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
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
        anyhow::bail!("runtime exited before readiness")
    })
    .await
    .context("runtime readiness timed out")??;
    let drain = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(line) = lines.next_line().await? {
            log.write_all(line.as_bytes()).await?;
            log.write_all(b"\n").await?;
        }
        Ok(())
    }));
    Ok((ready, drain))
}

#[cfg(unix)]
async fn read_status(cli: &Path, home: &Path, port: u16) -> Result<Value> {
    let graphql = ports::graphql_url(port);
    let output = Command::new(cli)
        .envs(
            support::homes::ProcessHomes::under(home.parent().context("runtime home parent")?)
                .env(),
        )
        .args(["status", "--home"])
        .arg(home)
        .arg("--graphql")
        .arg(&graphql)
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "runtime status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ready: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(ready["readiness_status"] == "ready", "{ready}");
    Ok(ready)
}

/// On Windows, `ConsoleChild` owns ConPTY output, so readiness is observed
/// across processes against the runtime's HTTP/GraphQL status endpoint.
#[cfg(windows)]
async fn wait_for_status(child: &mut Child, cli: &Path, home: &Path, port: u16) -> Result<Value> {
    let graphql = ports::graphql_url(port);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(
                child.try_wait()?.is_none(),
                "runtime exited before readiness"
            );
            let output = Command::new(cli)
                .envs(
                    support::homes::ProcessHomes::under(
                        home.parent().context("runtime home parent")?,
                    )
                    .env(),
                )
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
    tokio::time::timeout(
        Duration::from_secs(30),
        support::wait_for_listener(paths, || {
            ensure!(
                child.try_wait()?.is_none(),
                "client host exited before readiness"
            );
            Ok(())
        }),
    )
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
    // The runtime's own orderly teardown is longer than any one stage of it,
    // so a deadline picked here cannot tell a slow stop from a stalled one.
    let deadline = tokio::time::Instant::now() + gents::runtime::shutdown::shutdown_budget();
    #[cfg(unix)]
    let status = tokio::time::timeout_at(deadline, child.wait())
        .await
        .context("fixture process did not exit within the runtime's shutdown budget")??;
    #[cfg(windows)]
    let status = {
        let status = tokio::time::timeout_at(deadline, child.wait_for_exit())
            .await
            .context("fixture process did not exit within the runtime's shutdown budget")??;
        tokio::time::timeout_at(deadline, child.finish_output())
            .await
            .context("fixture process exited but console output cleanup did not finish")??;
        status
    };
    ensure!(status.success(), "fixture child exited with {status}");
    Ok(())
}
