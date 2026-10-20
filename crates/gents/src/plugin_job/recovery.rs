//! The observer's wake arm and recovery sweep for plugin job rows.

use std::sync::Arc;

use anyhow::{Context, Result};
use gents_protocol::job::JobPin;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use super::admit::{is_stalled, live_count, terminal_count, Refusal, StepRow};
use super::control::{kill, KillCause};
use super::driver::{load_lifecycle, read_step_row, JobDriver, StepOutcome};
use super::input::live_job_settings;
use super::origin::engine_guard;
use super::records::read_progress;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::plugin::store::{parse_coordinate, read_record};
use crate::request_binding::load_agent_request_by_doc_id;

/// Lean `PluginJobRow` for one job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryFacts {
    pub running: bool,
    pub session_stopped: bool,
    pub terminal_children: u32,
    pub observed: u32,
    pub live: u32,
    pub step_new: u32,
    pub deadline_expired: bool,
    pub available: Result<(), Refusal>,
    pub stepped: bool,
}

/// What one application of Lean's `pluginJobRecover` does to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickAction {
    Untouched,
    Kill(KillCause),
    Step,
}

/// Lean `pluginJobRecover`, first application: a row that is no longer running
/// is left alone; a stopped session cancels its job; a row past its deadline,
/// unavailable or stalled is failed closed; one with unobserved terminals, or at
/// rest before its first step since adoption, is re-driven one step; any other
/// row is left alone.
pub fn tick_action(facts: &RecoveryFacts) -> TickAction {
    if !facts.running {
        return TickAction::Untouched;
    }
    if facts.session_stopped {
        return TickAction::Kill(KillCause::Stop);
    }
    if facts.deadline_expired {
        return TickAction::Kill(KillCause::Deadline);
    }
    if let Err(refusal) = facts.available {
        return TickAction::Kill(KillCause::FailClosed(refusal));
    }
    if !facts.stepped && facts.live == 0 && facts.observed >= facts.terminal_children {
        return TickAction::Step;
    }
    if is_stalled(
        facts.live,
        facts.observed,
        facts.terminal_children,
        facts.step_new,
    ) {
        return TickAction::Kill(KillCause::FailClosed(Refusal::Stalled));
    }
    if facts.observed < facts.terminal_children {
        TickAction::Step
    } else {
        TickAction::Untouched
    }
}

/// Lean `pluginJobStepInvocationAllowed`: stale observers do not invoke a
/// guest after the canonical job row has already reached a terminal state.
pub fn step_invocation_allowed(row_running: bool, session_stopped: bool) -> bool {
    row_running && !session_stopped
}

/// Lean `pluginJobRecoverySweep`: one tick per running local job row, that is
/// `pluginJobRecover` applied twice. The first application kills the row or
/// re-drives one step; the second is the stall rule over the row that step
/// left, because admission reads the terminals the step had not yet observed
/// as news. A row whose step is already in flight is left to that step.
/// Returns the number of rows ticked.
pub async fn redrive_running_jobs(driver: &Arc<JobDriver>) -> Result<usize> {
    let runtime = driver.runtime();
    let access = ConfigAccess::local(runtime.node.clone());
    let local = match runtime.node.node_identity_did() {
        Some(did) => format!(r#", node_did: {{ _eq: "{}" }}"#, escape_graphql_string(did)),
        None => String::new(),
    };
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ lifecycle_state: {{ _eq: "running" }}{local} }}) {{ _docID request_doc_id job_pin }} }}"#
        ))
        .await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut ticked = 0;
    for row in rows {
        let (Some(row_doc_id), Some(request_doc_id), Some(pin)) = (
            row.get("_docID").and_then(Value::as_str),
            row.get("request_doc_id").and_then(Value::as_str),
            row.get("job_pin")
                .filter(|pin| !pin.is_null())
                .and_then(|pin| serde_json::from_value::<JobPin>(pin.clone()).ok()),
        ) else {
            continue;
        };
        match tick(driver, &access, row_doc_id, request_doc_id, &pin).await {
            Ok(true) => ticked += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                row_doc_id,
                error = %format!("{error:#}"),
                "plugin job recovery tick failed; the next sweep retries"
            ),
        }
    }
    Ok(ticked)
}

/// One tick of the row; `true` when it touched the row.
async fn tick(
    driver: &Arc<JobDriver>,
    access: &ConfigAccess,
    row_doc_id: &str,
    request_doc_id: &str,
    pin: &JobPin,
) -> Result<bool> {
    let (row, facts) = facts_of(driver, access, row_doc_id, request_doc_id, pin).await?;
    match tick_action(&facts) {
        TickAction::Untouched => Ok(false),
        TickAction::Kill(cause) => {
            kill_row(access, row_doc_id, &row, cause).await?;
            Ok(true)
        }
        TickAction::Step => {
            if let Some(outcome) = driver.step_now(row_doc_id).await {
                if outcome? == StepOutcome::Committed {
                    let (row, facts) =
                        facts_of(driver, access, row_doc_id, request_doc_id, pin).await?;
                    if let TickAction::Kill(cause) = tick_action(&facts) {
                        kill_row(access, row_doc_id, &row, cause).await?;
                    }
                }
            }
            Ok(true)
        }
    }
}

async fn kill_row(
    access: &ConfigAccess,
    row_doc_id: &str,
    row: &StepRow,
    cause: KillCause,
) -> Result<()> {
    let mut lifecycle = load_lifecycle(access, row_doc_id).await?;
    kill(access, &mut lifecycle, row, cause).await?;
    Ok(())
}

/// The row and its recovery facts as the stored state shows them now. A step
/// leaves `step_new` 0 in a stored row: its fresh spawns and messages are live
/// at its commit, so a row with nothing live and every terminal observed
/// added nothing. Adoption writes progress revision 1 and every committed
/// step raises it, so a row still at 1 has not stepped since adoption.
async fn facts_of(
    driver: &JobDriver,
    access: &ConfigAccess,
    row_doc_id: &str,
    request_doc_id: &str,
    pin: &JobPin,
) -> Result<(StepRow, RecoveryFacts)> {
    let runtime = driver.runtime();
    let row = read_step_row(access, row_doc_id).await?;
    let stepped = read_progress(access, row_doc_id)
        .await?
        .is_some_and(|progress| progress.revision > 1);
    let lifecycle = load_lifecycle(access, row_doc_id).await?;
    let session_stopped = crate::session::session_is_stopped(
        &runtime.node,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?;
    let available = if session_stopped {
        Err(Refusal::NotAvailable)
    } else {
        let caller = load_agent_request_by_doc_id(&runtime.node, request_doc_id)
            .await?
            .with_context(|| {
                format!("the caller request of job row {row_doc_id} does not exist")
            })?;
        let settings = live_job_settings(runtime, &caller, &pin.plugin_ref).await?;
        match parse_coordinate(&pin.plugin_ref)
            .ok()
            .and_then(|(namespace, name)| read_record(&runtime.home, namespace, name).ok())
        {
            Some(installed)
                if settings.is_some() && engine_guard(&runtime.home, &installed).is_ok() =>
            {
                if installed.digest == pin.engine_digest {
                    Ok(())
                } else {
                    Err(Refusal::EngineChanged)
                }
            }
            _ => Err(Refusal::NotAvailable),
        }
    };
    let deadline = pin.adopted_at + u64::from(pin.ceiling_at_pin.max_wall_secs) * 1000;
    let facts = RecoveryFacts {
        running: lifecycle.is_running(),
        session_stopped,
        terminal_children: terminal_count(&row) as u32,
        observed: row.observed_terminal,
        live: live_count(&row) as u32,
        step_new: 0,
        deadline_expired: deadline <= u64::try_from(chrono::Utc::now().timestamp_millis())?,
        available,
        stepped,
    };
    Ok((row, facts))
}

/// Observer arm: a request that reached a durable terminal may be a child of
/// a local running job row, which it names through
/// `caused_by_parent_tool_call_doc_id`; that row's driver is kicked. The step
/// the kick runs, never this arm, decides whether the row settles.
pub async fn wake_jobs_after_request_update(
    access: &ConfigAccess,
    driver: &JobDriver,
    request_doc_id: &str,
) -> Result<()> {
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state caused_by_parent_tool_call_doc_id }} }}"#,
            escape_graphql_string(request_doc_id)
        ))
        .await?;
    let Some(request) = response.pointer("/data/AgentRequest/0") else {
        return Ok(());
    };
    let terminal = RequestLifecycleState::is_terminal_str(
        request.get("lifecycle_state").and_then(Value::as_str),
    );
    let Some(row_doc_id) = request
        .get("caused_by_parent_tool_call_doc_id")
        .and_then(Value::as_str)
        .filter(|row| terminal && !row.is_empty())
    else {
        return Ok(());
    };
    // Embedded nodes carry an identity; a replicated row of another node is
    // not this runtime's to drive.
    let local = match driver.runtime().node.node_identity_did() {
        Some(did) => format!(r#", node_did: {{ _eq: "{}" }}"#, escape_graphql_string(did)),
        None => String::new(),
    };
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }}{local} }}, limit: 1) {{ job_pin }} }}"#,
            escape_graphql_string(row_doc_id)
        ))
        .await?;
    let is_job = response
        .pointer("/data/AgentToolCall/0/job_pin")
        .is_some_and(|pin| !pin.is_null());
    if is_job {
        driver.kick(row_doc_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::plugin_job::driver::tests::{start_running, stepper, write_step_tools, Stepper};
    use crate::plugin_job::driver::StepOutcome;
    use crate::plugin_job::input::tests::{node_did, request};
    use crate::plugin_job::keys::{job_session_id, job_spawn_key};
    use crate::plugin_job::records::read_pin;
    use gents_protocol::job::JobState;

    /// A request named `request_id` that `row` caused, in `state`, and its doc id.
    async fn child(
        step: &Stepper,
        row: &str,
        request_id: &str,
        state: &str,
        terminalized_at: Option<&str>,
    ) -> String {
        request(
            &step.job.node,
            row,
            request_id,
            &job_session_id(row, request_id, 0),
            &job_spawn_key(row, request_id, 0),
            "a",
            state,
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            terminalized_at,
        )
        .await;
        step.job
            .access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ _docID }} }}"#
            ))
            .await
            .unwrap()["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn terminal_child(step: &Stepper, row: &str, request_id: &str) -> String {
        child(
            step,
            row,
            request_id,
            "failed",
            Some("2026-01-01T00:01:00Z"),
        )
        .await
    }

    /// A second running job row of the fixture's plugin.
    async fn other_job_row(step: &mut Stepper) -> String {
        let row = step
            .job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        let pin = step.job.pin(&row, None);
        step.job.write_pin(&row, &pin).await;
        step.job.write_progress(&row, JobState::Running, 1).await;
        start_running(&step.job, &row).await;
        row
    }

    fn driver_of(step: &Stepper) -> Arc<JobDriver> {
        JobDriver::new(step.job.runtime.clone())
    }

    /// Waits until the kicked row's step has observed `count` terminal children.
    async fn observed(step: &Stepper, row: &str, count: u32) {
        for _ in 0..200 {
            if step.progress(row).await.observed_terminal_children >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("row {row} never observed {count} terminal children");
    }

    /// Long enough for a wrongly kicked step to have run.
    async fn settle_time() {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn terminal_child_request_kicks_exactly_its_job_row() {
        let mut step = stepper(json!({"effects": {}})).await;
        let other = other_job_row(&mut step).await;
        let request = terminal_child(&step, &step.row, "child-1").await;
        terminal_child(&step, &other, "child-2").await;
        let driver = driver_of(&step);
        wake_jobs_after_request_update(&step.job.access, &driver, &request)
            .await
            .unwrap();
        observed(&step, &step.row, 1).await;
        settle_time().await;
        assert_eq!(step.progress(&other).await.observed_terminal_children, 0);
        assert_eq!(step.progress(&other).await.revision, 1);
        step.close().await;
    }

    #[tokio::test]
    async fn wake_arm_never_settles_a_job_row() {
        let step = stepper(json!({"effects": {}})).await;
        let request = terminal_child(&step, &step.row, "child-1").await;
        let driver = driver_of(&step);
        wake_jobs_after_request_update(&step.job.access, &driver, &request)
            .await
            .unwrap();
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        observed(&step, &step.row, 1).await;
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        step.close().await;
    }

    #[tokio::test]
    async fn non_job_row_and_non_terminal_update_kick_nothing() {
        let mut step = stepper(json!({"effects": {}})).await;
        let native = step.job.row("bash", "call-3", json!({})).await;
        start_running(&step.job, &native).await;
        let driver = driver_of(&step);
        let pending = child(&step, &step.row, "child-1", "processing", None).await;
        let of_native = terminal_child(&step, &native, "child-2").await;
        let unparented = step
            .job
            .access
            .execute(r#"{ AgentRequest(filter: { request_id: { _eq: "request-1" } }) { _docID } }"#)
            .await
            .unwrap()["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        for request in [
            pending.as_str(),
            of_native.as_str(),
            unparented.as_str(),
            "no-such-doc",
        ] {
            wake_jobs_after_request_update(&step.job.access, &driver, request)
                .await
                .unwrap();
        }
        settle_time().await;
        assert!(driver.kicked.lock().unwrap().is_empty());
        let progress = step.progress(&step.row).await;
        assert_eq!(
            (progress.revision, progress.observed_terminal_children),
            (1, 0)
        );
        step.close().await;
    }

    fn now_ms() -> u64 {
        u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap()
    }

    /// A running job row beside `step`'s, pinned like it but adopted at
    /// `adopted_at` and on `engine_digest` (the installed engine's when `None`).
    async fn pinned_row(
        step: &mut Stepper,
        call_id: &str,
        adopted_at: u64,
        engine_digest: Option<&str>,
    ) -> String {
        let base = read_pin(&step.job.access, &step.row)
            .await
            .unwrap()
            .unwrap();
        let row = step
            .job
            .row("workflow", call_id, json!({"argv": ["run"]}))
            .await;
        let pin = JobPin {
            chain_root_doc_id: row.clone(),
            handle: format!("handle-{row}"),
            adopted_at,
            engine_digest: engine_digest.map_or(base.engine_digest.clone(), str::to_owned),
            ..base
        };
        step.job.write_pin(&row, &pin).await;
        step.job.write_progress(&row, JobState::Running, 1).await;
        start_running(&step.job, &row).await;
        row
    }

    /// What a re-driven step's fresh spawn leaves behind: the job fixture
    /// holds no signing identity, so a step cannot stage a session message,
    /// and the row is given the live child the spawn would have opened.
    async fn fresh_spawn(step: &Stepper, row: &str, request_id: &str) {
        child(step, row, request_id, "processing", None).await;
    }

    #[tokio::test]
    async fn deadline_expired_row_ends_timed_out() {
        let mut step = stepper(json!({"effects": {}})).await;
        let expired = pinned_row(&mut step, "call-2", 1, None).await;
        let driver = driver_of(&step);
        redrive_running_jobs(&driver).await.unwrap();
        assert_eq!(step.row_json(&expired).await["lifecycle_state"], "timedOut");
        assert_eq!(step.output(&expired).await["code"], "plugin_job_timed_out");
        step.close().await;
    }

    #[tokio::test]
    async fn row_with_a_vanished_ceiling_or_changed_engine_fails_closed() {
        let mut step = stepper(json!({"effects": {}})).await;
        let driver = driver_of(&step);
        let changed = pinned_row(&mut step, "call-2", now_ms(), Some("sha256:other")).await;
        redrive_running_jobs(&driver).await.unwrap();
        assert_eq!(step.row_json(&changed).await["lifecycle_state"], "failed");
        assert_eq!(
            step.output(&changed).await["code"],
            "plugin_job_engine_changed"
        );
        let vanished = pinned_row(&mut step, "call-3", now_ms(), None).await;
        write_step_tools(&step.job, None).await;
        redrive_running_jobs(&driver).await.unwrap();
        assert_eq!(step.row_json(&vanished).await["lifecycle_state"], "failed");
        assert_eq!(
            step.output(&vanished).await["code"],
            "plugin_job_not_available"
        );
        assert_eq!(step.progress(&vanished).await.revision, 1);
        step.close().await;
    }

    #[tokio::test]
    async fn row_with_an_unobserved_terminal_child_is_restepped() {
        let step = stepper(json!({"effects": {}})).await;
        terminal_child(&step, &step.row, "child-1").await;
        fresh_spawn(&step, &step.row, "live-1").await;
        let driver = driver_of(&step);
        assert_eq!(redrive_running_jobs(&driver).await.unwrap(), 1);
        let progress = step.progress(&step.row).await;
        assert_eq!(
            (progress.revision, progress.observed_terminal_children),
            (2, 1)
        );
        step.close().await;
    }

    #[tokio::test]
    async fn restart_mid_child_keeps_the_row_running_and_never_process_lost() {
        let step = stepper(json!({"effects": {}})).await;
        child(&step, &step.row, "child-1", "processing", None).await;
        let driver = driver_of(&step);
        assert_eq!(redrive_running_jobs(&driver).await.unwrap(), 0);
        let registry = crate::hook::BackgroundExecutionRegistry::default();
        crate::periodic_recovery::run_periodic_recovery_sweeps(
            &step.job.node,
            node_did(),
            &registry,
        )
        .await
        .unwrap();
        settle_time().await;
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        assert_eq!(step.progress(&step.row).await.revision, 1);
        step.close().await;
    }

    #[tokio::test]
    async fn at_rest_row_with_nothing_live_or_unobserved_fails_closed_stalled() {
        let step = stepper(json!({"effects": {}})).await;
        let driver = driver_of(&step);
        assert_eq!(redrive_running_jobs(&driver).await.unwrap(), 1);
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(step.output(&step.row).await["code"], "plugin_job_stalled");
        assert_eq!(step.progress(&step.row).await.revision, 1);
        step.close().await;
    }

    #[tokio::test]
    async fn restepped_row_whose_step_has_no_new_effects_fails_closed_stalled() {
        let step = stepper(json!({"effects": {}})).await;
        terminal_child(&step, &step.row, "child-1").await;
        let driver = driver_of(&step);
        assert_eq!(redrive_running_jobs(&driver).await.unwrap(), 1);
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(step.output(&step.row).await["code"], "plugin_job_stalled");
        assert_eq!(step.progress(&step.row).await.revision, 2);
        step.close().await;
    }

    #[tokio::test]
    async fn restepped_row_whose_step_spawns_keeps_running() {
        let step = stepper(json!({"effects": {}})).await;
        terminal_child(&step, &step.row, "child-1").await;
        fresh_spawn(&step, &step.row, "live-1").await;
        let driver = driver_of(&step);
        redrive_running_jobs(&driver).await.unwrap();
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
        settle_time().await;
        assert_eq!(step.progress(&step.row).await.revision, 2);
        step.close().await;
    }

    /// A row a resume just adopted: its chain's one child is terminal and was
    /// observed before the resume, nothing is live, and no step has committed
    /// since its adoption (progress revision 1).
    async fn resumed_row(step: &mut Stepper) -> String {
        let row = step
            .job
            .row("workflow", "call-2", json!({"argv": ["resume", "flow"]}))
            .await;
        let base = read_pin(&step.job.access, &step.row)
            .await
            .unwrap()
            .unwrap();
        let pin = JobPin {
            chain_root_doc_id: row.clone(),
            resume_of: Some(step.row.clone()),
            adopted_at: now_ms(),
            ..base
        };
        step.job.write_pin(&row, &pin).await;
        terminal_child(step, &row, "child-1").await;
        step.job
            .write_progress_observed(&row, JobState::Running, 1, 1)
            .await;
        start_running(&step.job, &row).await;
        row
    }

    #[tokio::test]
    async fn resumed_row_before_its_first_step_is_stepped_not_killed_stalled() {
        let mut step =
            stepper(json!({"suspend": {"kind": "await_user", "message": "which one?"}})).await;
        let resumed = resumed_row(&mut step).await;
        let driver = driver_of(&step);
        redrive_running_jobs(&driver).await.unwrap();
        assert_eq!(
            step.row_json(&resumed).await["lifecycle_state"],
            "completed",
            "{}",
            step.output(&resumed).await
        );
        assert_eq!(step.output(&resumed).await["status"], "awaiting_user");
        step.close().await;
    }

    #[tokio::test]
    async fn stepped_row_at_rest_still_fails_closed_stalled() {
        let step = stepper(json!({"effects": {}})).await;
        terminal_child(&step, &step.row, "child-1").await;
        let driver = driver_of(&step);
        let stepped = driver.step_now(&step.row).await.unwrap().unwrap();
        assert_eq!(stepped, StepOutcome::Committed);
        assert_eq!(redrive_running_jobs(&driver).await.unwrap(), 1);
        assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
        assert_eq!(step.output(&step.row).await["code"], "plugin_job_stalled");
        assert_eq!(step.progress(&step.row).await.revision, 2);
        step.close().await;
    }

    #[tokio::test]
    async fn one_tick_steps_each_row_at_most_once() {
        let mut step = stepper(json!({"effects": {}})).await;
        let other = pinned_row(&mut step, "call-2", now_ms(), None).await;
        terminal_child(&step, &step.row, "child-1").await;
        fresh_spawn(&step, &step.row, "live-1").await;
        terminal_child(&step, &other, "child-2").await;
        fresh_spawn(&step, &other, "live-2").await;
        let driver = driver_of(&step);
        assert_eq!(redrive_running_jobs(&driver).await.unwrap(), 2);
        settle_time().await;
        for row in [&step.row, &other] {
            assert_eq!(step.row_json(row).await["lifecycle_state"], "running");
            assert_eq!(step.progress(row).await.revision, 2);
        }
        let busy = pinned_row(&mut step, "call-3", now_ms(), None).await;
        fresh_spawn(&step, &busy, "live-3").await;
        let (first, second) = tokio::join!(driver.step_now(&busy), driver.step_now(&busy));
        assert!(matches!(first, Some(Ok(StepOutcome::Committed))));
        assert!(second.is_none());
        observed_revision(&step, &busy, 3).await;
        settle_time().await;
        assert_eq!(step.progress(&busy).await.revision, 3);
        step.close().await;
    }

    /// Waits until the row's progress reaches `revision`.
    async fn observed_revision(step: &Stepper, row: &str, revision: u64) {
        for _ in 0..200 {
            if step.progress(row).await.revision >= revision {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("row {row} never reached revision {revision}");
    }
}
