use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::collection::Collection;
use crate::config_client::config_projection;
use crate::document_config::{Task, TaskHook, TaskHookPhase};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::lifecycle::RequestTerminalOutcome;
use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
use crate::watcher::AgentRequest;

#[path = "task_hooks/records.rs"]
mod records;
#[cfg(test)]
#[path = "task_hooks/tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) use records::RecordedHookAttempt;
pub(crate) use records::TaskHookRecordHandle;
pub(crate) use records::{recover_task_hook_records, TaskHookRecord, TaskHookRecordStore};

/// `TaskHooks.defaultHookTimeoutSecs`: the executor's own bound for a hook that
/// configures none. Callers consume [`effective_timeout_secs`] rather than
/// re-deriving it.
pub(crate) const DEFAULT_TASK_HOOK_TIMEOUT_SECS: u64 = 120;

/// Captured stdout/stderr retained per attempt so a failing gate can name what
/// the command said without holding an unbounded host stream in memory.
const HOOK_OUTPUT_BYTE_CAP: usize = 64 * 1024;

/// Hook output copied into a request's `failure_reason`. That field replicates
/// to every peer with the request, so only the tail of the output is kept.
const FAILURE_REASON_OUTPUT_CAP: usize = 4 * 1024;

/// `TaskHook.effectiveTimeout`. A nonpositive configured value is rejected by
/// `Task::validate`, so it cannot reach execution; this function still mirrors
/// the model there rather than substituting the default.
pub(crate) fn effective_timeout_secs(hook: &TaskHook) -> u64 {
    match hook.timeout_secs {
        None => DEFAULT_TASK_HOOK_TIMEOUT_SECS,
        Some(configured) => configured.max(0) as u64,
    }
}

/// `TaskHooks.CommandResult`. `Exited { code: None }` is a signal-terminated
/// child: no exit status exists, so it can never satisfy the modeled success
/// condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HookCommandResult {
    Exited {
        code: Option<i64>,
    },
    LaunchFailed,
    TimedOut,
    /// Cancelled, or an outcome the executor cannot observe. Terminal and
    /// reported, never retried.
    Interrupted,
}

impl HookCommandResult {
    pub(crate) fn succeeded(&self) -> bool {
        matches!(self, Self::Exited { code: Some(0) })
    }

    fn describe(&self) -> String {
        match self {
            Self::Exited { code: Some(code) } => format!("exited with status {code}"),
            Self::Exited { code: None } => "was terminated by a signal".to_string(),
            Self::LaunchFailed => "failed to launch".to_string(),
            Self::TimedOut => "exceeded its timeout".to_string(),
            Self::Interrupted => "was interrupted".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HookAttempt {
    pub(crate) hook_id: String,
    pub(crate) result: HookCommandResult,
    /// Operator-facing detail: captured output, or the launch error.
    pub(crate) detail: String,
}

/// `TaskHooks.PrimaryError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookPrimaryError {
    Hook(String),
    Agent,
}

/// `TaskHooks.TaskOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum TaskHookOutcome {
    #[default]
    Success,
    Failure(HookPrimaryError),
    Interrupted,
}

impl TaskHookOutcome {
    /// `TaskOutcome.toRequestState`, expressed through the existing request
    /// terminal owner rather than a second terminal vocabulary.
    pub(crate) fn terminal_outcome(&self) -> RequestTerminalOutcome {
        match self {
            Self::Success => RequestTerminalOutcome::Completed,
            Self::Failure(_) => RequestTerminalOutcome::Failed,
            Self::Interrupted => RequestTerminalOutcome::Interrupted,
        }
    }
}

/// `TaskHooks.AgentResult`: one observation of the owned execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskAgentResult {
    Success,
    Failure,
    Cancelled,
    Interrupted,
}

/// `TaskHooks.RunResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TaskHookRun {
    pub(crate) outcome: TaskHookOutcome,
    pub(crate) before_attempted: Vec<HookAttempt>,
    pub(crate) after_success_attempted: Vec<HookAttempt>,
    pub(crate) after_failure_attempted: Vec<HookAttempt>,
    pub(crate) finally_attempted: Vec<HookAttempt>,
    pub(crate) agent_result: Option<TaskAgentResult>,
}

impl TaskHookRun {
    /// `RunResult.cleanupErrors`.
    pub(crate) fn cleanup_errors(&self) -> Vec<String> {
        self.finally_attempted
            .iter()
            .filter(|attempt| !attempt.result.succeeded())
            .map(|attempt| attempt.hook_id.clone())
            .collect()
    }

    /// `RunResult.finalOutcome`.
    pub(crate) fn final_outcome(&self) -> TaskHookOutcome {
        match &self.outcome {
            TaskHookOutcome::Success => phase_outcome(&self.finally_attempted),
            other => other.clone(),
        }
    }

    /// What the operator's gate said, for the durable failure reason the
    /// terminal owner records.
    pub(crate) fn hook_failure_reason(&self, hook_id: &str) -> String {
        let attempt = self
            .before_attempted
            .iter()
            .chain(&self.after_success_attempted)
            .chain(&self.after_failure_attempted)
            .chain(&self.finally_attempted)
            .find(|attempt| attempt.hook_id == hook_id && !attempt.result.succeeded());
        match attempt {
            None => format!("task hook {hook_id} failed"),
            Some(attempt) => {
                let mut reason = format!("task hook {hook_id} {}", attempt.result.describe());
                if !attempt.detail.is_empty() {
                    reason.push('\n');
                    reason.push_str(&failure_reason_tail(&attempt.detail));
                }
                reason
            }
        }
    }
}

fn failure_reason_tail(detail: &str) -> String {
    let tail = crate::streaming::tail_window(detail, FAILURE_REASON_OUTPUT_CAP);
    if tail.len() == detail.len() {
        tail.to_owned()
    } else {
        format!("…{tail}")
    }
}

/// `TaskHooks.HookExec`: one attempt observation per configured occurrence.
/// Cwd, environment, launch, capture, timeout and process termination stay with
/// the host execution owner; implementations translate its outcome.
#[async_trait::async_trait]
pub(crate) trait TaskHookExec: Send + Sync {
    async fn attempt(&self, hook: &TaskHook) -> HookAttempt;
}

fn hooks_of_phase(hooks: &[TaskHook], phase: TaskHookPhase) -> impl Iterator<Item = &TaskHook> {
    hooks.iter().filter(move |hook| hook.phase == phase)
}

/// `TaskHooks.runPhase`.
async fn run_phase(
    hooks: &[TaskHook],
    phase: TaskHookPhase,
    exec: &dyn TaskHookExec,
) -> Vec<HookAttempt> {
    let mut attempts = Vec::new();
    for hook in hooks_of_phase(hooks, phase) {
        let attempt = exec.attempt(hook).await;
        let succeeded = attempt.result.succeeded();
        attempts.push(attempt);
        if !succeeded {
            break;
        }
    }
    attempts
}

/// `TaskHooks.runFinally`.
async fn run_finally(hooks: &[TaskHook], exec: &dyn TaskHookExec) -> Vec<HookAttempt> {
    let mut attempts = Vec::new();
    for hook in hooks_of_phase(hooks, TaskHookPhase::Finally) {
        attempts.push(exec.attempt(hook).await);
    }
    attempts
}

/// `TaskHooks.phaseOutcome`.
fn phase_outcome(attempts: &[HookAttempt]) -> TaskHookOutcome {
    match attempts.iter().find(|attempt| !attempt.result.succeeded()) {
        None => TaskHookOutcome::Success,
        Some(attempt) => match attempt.result {
            HookCommandResult::Interrupted => TaskHookOutcome::Interrupted,
            _ => TaskHookOutcome::Failure(HookPrimaryError::Hook(attempt.hook_id.clone())),
        },
    }
}

/// `TaskHooks.runTask`: the single orchestration. Preparation decides whether
/// the owned execution runs, its observation selects one ordinary after-phase,
/// then every cleanup hook is attempted.
pub(crate) async fn run_task_hooks<Work, Fut>(
    hooks: &[TaskHook],
    exec: &dyn TaskHookExec,
    work: Work,
) -> TaskHookRun
where
    Work: FnOnce() -> Fut,
    Fut: std::future::Future<Output = TaskAgentResult>,
{
    let mut run = TaskHookRun {
        before_attempted: run_phase(hooks, TaskHookPhase::Before, exec).await,
        ..Default::default()
    };
    match phase_outcome(&run.before_attempted) {
        TaskHookOutcome::Interrupted => run.outcome = TaskHookOutcome::Interrupted,
        TaskHookOutcome::Failure(error) => {
            run.outcome = TaskHookOutcome::Failure(error);
            run.after_failure_attempted = run_phase(hooks, TaskHookPhase::AfterFailure, exec).await;
        }
        TaskHookOutcome::Success => {
            let agent = work().await;
            run.agent_result = Some(agent);
            match agent {
                TaskAgentResult::Cancelled | TaskAgentResult::Interrupted => {
                    run.outcome = TaskHookOutcome::Interrupted
                }
                TaskAgentResult::Success => {
                    let after = run_phase(hooks, TaskHookPhase::AfterSuccess, exec).await;
                    run.outcome = phase_outcome(&after);
                    run.after_success_attempted = after;
                }
                TaskAgentResult::Failure => {
                    run.outcome = TaskHookOutcome::Failure(HookPrimaryError::Agent);
                    run.after_failure_attempted =
                        run_phase(hooks, TaskHookPhase::AfterFailure, exec).await;
                }
            }
        }
    }
    run.finally_attempted = run_finally(hooks, exec).await;
    run
}

/// Cancellation sources for one task execution's hooks. A user interrupt
/// cancels ordinary-phase commands only; runtime shutdown cancels every phase
/// (`TaskHooks.runFinally`).
#[derive(Clone)]
pub(crate) struct TaskHookCancellation {
    shutdown: CancellationToken,
    interrupt: CancellationToken,
}

impl Default for TaskHookCancellation {
    fn default() -> Self {
        Self::under(&CancellationToken::new())
    }
}

impl TaskHookCancellation {
    /// Cancellation that runtime shutdown, `shutdown`, also cancels.
    pub(crate) fn under(shutdown: &CancellationToken) -> Self {
        let shutdown = shutdown.child_token();
        let interrupt = shutdown.child_token();
        Self {
            shutdown,
            interrupt,
        }
    }

    pub(crate) fn interrupt(&self) {
        self.interrupt.cancel();
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.cancel();
    }

    fn for_phase(&self, phase: TaskHookPhase) -> &CancellationToken {
        match phase {
            TaskHookPhase::Finally => &self.shutdown,
            TaskHookPhase::Before | TaskHookPhase::AfterSuccess | TaskHookPhase::AfterFailure => {
                &self.interrupt
            }
        }
    }

    /// Cancels hooks when the request's interrupt observer latches, the
    /// execution lease owner observes this execution lost the request (a
    /// revocation writes no interrupt latch), or the runtime shuts down. The
    /// first two cancel ordinary phases only. A closed channel stops being
    /// watched; its last value still counts.
    pub(crate) fn follow(
        &self,
        mut interrupt: tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        ownership_lost: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        if *shutdown.borrow() {
            self.shutdown();
        } else if interrupt.borrow().is_some() || ownership_lost.is_cancelled() {
            self.interrupt();
        }
        let cancellation = self.clone();
        tokio::spawn(async move {
            let ordinary = async {
                tokio::select! {
                    Ok(_) = interrupt.wait_for(Option::is_some) => {}
                    () = ownership_lost.cancelled() => {}
                }
                cancellation.interrupt();
            };
            let all = async {
                if shutdown.wait_for(|stopping| *stopping).await.is_ok() {
                    cancellation.shutdown();
                }
            };
            tokio::join!(ordinary, all);
        })
    }
}

/// Runs one configured occurrence through the host execution owner. The cwd is
/// the behavior's already-admitted host-tools root, so a hook cannot select a
/// workspace overlay of its own.
///
/// Hook time is spent inside the request deadline fixed at claim. A hook's
/// own timeout is not shortened to fit it, so a `before` hook that outlives
/// the deadline leaves the owned work to fail with the owned loop's deadline
/// error rather than starting late.
pub(crate) struct ManagedTaskHookExec {
    cwd: PathBuf,
    cancellation: TaskHookCancellation,
    record: Option<TaskHookRecordHandle>,
    execution: Option<(
        std::sync::Arc<EmbeddedNode>,
        String,
        crate::lifecycle::RequestExecutionLease,
    )>,
}

impl ManagedTaskHookExec {
    pub(crate) fn new(cwd: PathBuf, cancellation: TaskHookCancellation) -> Self {
        Self {
            cwd,
            cancellation,
            record: None,
            execution: None,
        }
    }

    pub(crate) fn with_execution_lease(
        mut self,
        node: std::sync::Arc<EmbeddedNode>,
        request_doc_id: String,
        lease: crate::lifecycle::RequestExecutionLease,
    ) -> Self {
        self.execution = Some((node, request_doc_id, lease));
        self
    }

    /// Records each attempt in `record` before its command launches.
    pub(crate) fn with_record(mut self, record: Option<TaskHookRecordHandle>) -> Self {
        self.record = record;
        self
    }
}

fn detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut detail = String::new();
    for stream in [stdout, stderr] {
        let text = String::from_utf8_lossy(stream);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(text);
    }
    detail
}

#[async_trait::async_trait]
impl TaskHookExec for ManagedTaskHookExec {
    /// A command whose cancellation already fired is not launched at all, so it has no host
    /// effect to account for and is not recorded; one whose attempt cannot be
    /// recorded is not launched either, since recovery could then replay it.
    async fn attempt(&self, hook: &TaskHook) -> HookAttempt {
        let refused = |result, detail: String| HookAttempt {
            hook_id: hook.hook_id.clone(),
            result,
            detail,
        };
        // Admission bounds the timeout (`TaskHooks.admitted_timeout_bounded`).
        let deadline_at =
            Utc::now() + chrono::Duration::seconds(effective_timeout_secs(hook) as i64);
        let cancellation = self.cancellation.for_phase(hook.phase);
        if cancellation.is_cancelled() {
            return refused(
                HookCommandResult::Interrupted,
                "cancelled before launch".to_string(),
            );
        }
        if let Some(record) = &self.record {
            if let Err(error) = record.attempt_started(&hook.hook_id).await {
                return refused(
                    HookCommandResult::LaunchFailed,
                    format!("could not record the attempt before launch: {error}"),
                );
            }
        }
        // TaskHooks.ownershipCheckedExec: read after the durable attempt write,
        // so that host I/O cannot age the launch's ownership observation.
        let refusal = if hook.phase != TaskHookPhase::Finally {
            match &self.execution {
                Some((node, request_doc_id, lease)) => match lease
                    .owns_execution(node, request_doc_id)
                    .await
                {
                    Ok(true) => None,
                    Ok(false) => {
                        self.cancellation.interrupt();
                        Some(refused(
                            HookCommandResult::Interrupted,
                            "execution ownership lost before launch".to_owned(),
                        ))
                    }
                    Err(error) => Some(refused(
                        HookCommandResult::LaunchFailed,
                        format!("could not verify execution ownership before launch: {error:#}"),
                    )),
                },
                None => None,
            }
        } else {
            None
        };
        let refusal = refusal.or_else(|| {
            cancellation.is_cancelled().then(|| {
                refused(
                    HookCommandResult::Interrupted,
                    "cancelled before launch".to_owned(),
                )
            })
        });
        if let Some(attempt) = refusal {
            if let Some(record) = &self.record {
                record
                    .attempt_finished(&hook.hook_id, &attempt.result)
                    .await;
            }
            return attempt;
        }
        let execution = run_managed_exec(ManagedExecRequest {
            argv: hook.command.clone(),
            cwd: self.cwd.clone(),
            deadline_at: Some(deadline_at),
            cancellation_token: cancellation.clone(),
            max_output_bytes: HOOK_OUTPUT_BYTE_CAP,
            stdin: Vec::new(),
            environment: None,
            tool_name: Some(format!("task_hook:{}", hook.hook_id)),
            live_output: None,
        });
        let outcome = match &self.record {
            Some(record) => {
                crate::managed_exec::ownership::scope_process_recorder(
                    record.process_recorder(&hook.hook_id),
                    execution,
                )
                .await
            }
            None => execution.await,
        };
        let (result, detail) = match outcome {
            ManagedExecOutcome::Exited {
                code,
                stdout,
                stderr,
                ..
            } => (
                HookCommandResult::Exited {
                    code: code.map(i64::from),
                },
                detail(&stdout, &stderr),
            ),
            ManagedExecOutcome::TimedOut { stdout, stderr, .. } => {
                (HookCommandResult::TimedOut, detail(&stdout, &stderr))
            }
            ManagedExecOutcome::Cancelled { stdout, stderr, .. } => {
                (HookCommandResult::Interrupted, detail(&stdout, &stderr))
            }
            ManagedExecOutcome::SpawnFailed { error } => (HookCommandResult::LaunchFailed, error),
        };
        if let Some(record) = &self.record {
            record.attempt_finished(&hook.hook_id, &result).await;
        }
        HookAttempt {
            hook_id: hook.hook_id.clone(),
            result,
            detail,
        }
    }
}

/// The hooks of the Task a claimed request was fired from. The Task fire
/// receipt written with the request names it for scheduled, event and manual
/// runs; an automated request without a receipt falls back to its Trigger's
/// Task. A goal-backed Task's receipt names only its opening request, so its
/// continuations run without hooks.
///
/// A request bound to a Task that is gone or disabled by claim time fails:
/// running it without the hooks it was fired under would skip the operator's
/// gates, so the binding fails closed. Claim admission already refuses an
/// automated request whose Trigger is gone.
pub(crate) async fn resolve_request_task_hooks(
    node: &EmbeddedNode,
    request: &AgentRequest,
) -> Result<Vec<TaskHook>> {
    let task_id = match load_fire_task_id(node, &request.node_did, &request.request_id).await? {
        Some(task_id) => Some(task_id),
        None if request.has_automated_trigger_lineage() => {
            let trigger_id = request
                .caused_by_trigger_id
                .as_deref()
                .context("automated trigger lineage has no trigger_id")?;
            load_trigger_task_id(node, &request.node_did, trigger_id).await?
        }
        None => None,
    };
    let Some(task_id) = task_id else {
        return Ok(Vec::new());
    };
    let task = load_task(node, &request.node_did, &task_id)
        .await?
        .with_context(|| {
            format!("Task {task_id} this request was fired from no longer exists; refusing to run it without its hooks")
        })?;
    anyhow::ensure!(
        task.enabled,
        "Task {task_id} this request was fired from is disabled; refusing to run it"
    );
    task.validate()
        .with_context(|| format!("Task {task_id} hooks are not admissible"))?;
    Ok(task.hooks)
}

fn nonempty_task_id(task_id: Option<String>) -> Option<String> {
    task_id
        .map(|task_id| task_id.trim().to_owned())
        .filter(|task_id| !task_id.is_empty())
}

#[derive(Deserialize)]
struct TaskIdRow {
    task_id: Option<String>,
}

async fn load_fire_task_id(
    node: &EmbeddedNode,
    node_did: &str,
    request_id: &str,
) -> Result<Option<String>> {
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ TriggerFire(filter: {{ owner_did: {{ _eq: "{}" }}, request_id: {{ _eq: "{}" }} }}, limit: 2) {{ task_id }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(request_id),
        ),
        "load task hook fire receipt",
    )
    .await?;
    let rows: Vec<TaskIdRow> = crate::graphql::rows(&response, "TriggerFire")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "request {request_id:?} has multiple Task fire receipts"
    );
    Ok(nonempty_task_id(
        rows.into_iter().next().and_then(|row| row.task_id),
    ))
}

async fn load_trigger_task_id(
    node: &EmbeddedNode,
    node_did: &str,
    trigger_id: &str,
) -> Result<Option<String>> {
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 2) {{ task_id }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(trigger_id),
        ),
        "load task hook trigger",
    )
    .await?;
    let rows: Vec<TaskIdRow> = crate::graphql::rows(&response, "Trigger")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous Trigger {trigger_id:?} for {node_did:?}"
    );
    Ok(nonempty_task_id(
        rows.into_iter().next().and_then(|row| row.task_id),
    ))
}

async fn load_task(node: &EmbeddedNode, node_did: &str, task_id: &str) -> Result<Option<Task>> {
    let (fields, _) = config_projection(Collection::Task, None)?;
    let response = graphql_with_transaction_retry(
        node,
        &format!(
            "{{ Task(filter: {{ node_did: {{_eq: \"{}\"}}, task_id: {{_eq: \"{}\"}} }}, limit: 2) {{ {} }} }}",
            escape_graphql_string(node_did),
            escape_graphql_string(task_id),
            fields.join(" "),
        ),
        "load task hooks",
    )
    .await?;
    let rows: Vec<serde_json::Value> = crate::graphql::rows(&response, "Task")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous Task {task_id:?} for {node_did:?}"
    );
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let (_, canonical) = config_projection(Collection::Task, Some(&row))?;
    let canonical = canonical.context("Task row has no canonical configuration")?;
    serde_json::from_value(canonical)
        .with_context(|| format!("decoding Task {task_id:?}"))
        .map(Some)
}
