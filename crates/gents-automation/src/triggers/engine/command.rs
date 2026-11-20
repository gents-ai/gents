//! Command Tasks: a fire runs the Task's host command on the Node through the
//! task-hook runner. It writes no AgentRequest and starts no model turn; the
//! run is recorded on the Trigger that fired it (`Trigger.last_run`).

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rapidhash::RapidHashMap;
use tokio::sync::watch;

use gents_documents::task_delivery::{validate_command_delivery, TriggerKind};
use gents_documents::{
    CommandRun, CommandRunState, ConcurrencyMode, TaskCommand, TaskHook, TaskHookPhase,
};
use gents_toolcalls::call::task_hooks::{
    HookCommandResult, ManagedTaskHookExec, TaskHookCancellation, TaskHookExec,
};

use super::{FireIntent, FireResult, TriggerEngine, SERIAL_BUSY};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Lean `Triggers.CommandRun.Admission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAdmission {
    Start,
    SkipBusy,
    SupersedeThenStart,
    RefuseQueued,
}

/// Lean `Triggers.CommandRun.admit`.
pub fn admit_command(concurrency: ConcurrencyMode, running: bool) -> CommandAdmission {
    match (concurrency, running) {
        (ConcurrencyMode::QueuedSerial, _) => CommandAdmission::RefuseQueued,
        (ConcurrencyMode::Serial, true) => CommandAdmission::SkipBusy,
        (ConcurrencyMode::LatestOnly, true) => CommandAdmission::SupersedeThenStart,
        (ConcurrencyMode::Parallel, _)
        | (ConcurrencyMode::Serial, false)
        | (ConcurrencyMode::LatestOnly, false) => CommandAdmission::Start,
    }
}

/// What a command fire needs from the Node: the folder it runs in and the
/// Trigger documents it records on.
pub trait CommandHost: Send + Sync {
    /// The folder the command runs in, checked against WorkspaceRoot policy
    /// at each launch.
    fn command_root(&self) -> BoxFuture<'_, anyhow::Result<PathBuf>>;

    fn record_run<'a>(
        &'a self,
        node_did: &'a str,
        trigger_id: &'a str,
        run: &'a CommandRun,
    ) -> BoxFuture<'a, anyhow::Result<()>>;

    fn stop_for_budget<'a>(
        &'a self,
        node_did: &'a str,
        trigger_id: &'a str,
        reason: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// Runs commands in the operator's tool root and records on the Node's own
/// Trigger rows.
pub(crate) struct ProductionCommandHost {
    node: Arc<defra_node::Node>,
    ceiling_root: Option<PathBuf>,
}

impl ProductionCommandHost {
    pub(crate) fn new(node: Arc<defra_node::Node>, ceiling_root: Option<PathBuf>) -> Self {
        Self { node, ceiling_root }
    }
}

impl CommandHost for ProductionCommandHost {
    fn command_root(&self) -> BoxFuture<'_, anyhow::Result<PathBuf>> {
        Box::pin(gents_tools::surface::node_command_root(
            &self.node,
            self.ceiling_root.as_deref(),
        ))
    }

    fn record_run<'a>(
        &'a self,
        node_did: &'a str,
        trigger_id: &'a str,
        run: &'a CommandRun,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(gents_documents::record_trigger_command_run(
            &self.node, node_did, trigger_id, run,
        ))
    }

    fn stop_for_budget<'a>(
        &'a self,
        node_did: &'a str,
        trigger_id: &'a str,
        reason: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(gents_documents::stop_trigger_for_budget(
            &self.node, node_did, trigger_id, reason,
        ))
    }
}

struct RunningCommand {
    run_id: String,
    cancellation: TaskHookCancellation,
    done: watch::Receiver<bool>,
}

/// The runtime's own record of the commands its fires launched: a process
/// lives only as long as the runtime, so this is the in-flight marker.
#[derive(Default)]
pub(crate) struct CommandRuns {
    running: RapidHashMap<String, Vec<RunningCommand>>,
    admitted: RapidHashMap<String, VecDeque<AdmittedFire>>,
}

/// A fire admitted within the last hour: it counts against the Trigger's
/// budget, and a source re-emitting its fire before its cursor advanced is a
/// duplicate of it rather than a second run.
struct AdmittedFire {
    at: DateTime<Utc>,
    fire_key: String,
    run_id: String,
}

/// Where a run records itself: its Trigger, or nowhere for a manual fire.
#[derive(Clone)]
struct RunRecord {
    host: Arc<dyn CommandHost>,
    node_did: String,
    trigger_id: Option<String>,
}

impl RunRecord {
    async fn write(&self, run: &CommandRun) {
        let Some(trigger_id) = &self.trigger_id else {
            return;
        };
        if let Err(error) = self.host.record_run(&self.node_did, trigger_id, run).await {
            tracing::warn!(%trigger_id, run_id = %run.run_id, %error, "failed to record command Task run");
        }
    }
}

fn finished_state(result: &HookCommandResult) -> CommandRunState {
    match result {
        HookCommandResult::Exited { code: Some(0) } => CommandRunState::Succeeded,
        HookCommandResult::Exited { .. } | HookCommandResult::LaunchFailed => {
            CommandRunState::Failed
        }
        HookCommandResult::TimedOut => CommandRunState::TimedOut,
        HookCommandResult::Interrupted => CommandRunState::Interrupted,
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn reject(intent: FireIntent, result: FireResult) -> FireResult {
    (intent.on_result)(result.clone());
    result
}

impl TriggerEngine {
    pub(super) async fn dispatch_command(
        &self,
        intent: FireIntent,
        command: TaskCommand,
    ) -> FireResult {
        let snapshot = self.snapshot_rx.borrow().clone();
        let schedule = match intent.trigger_id.as_deref() {
            Some(trigger_id) if intent.trigger_kind != TriggerKind::Event => {
                match snapshot.active_schedules().get(trigger_id) {
                    Some(schedule) => Some(schedule.clone()),
                    None => {
                        return reject(
                            intent,
                            FireResult::Skipped {
                                reason: "trigger disabled".into(),
                            },
                        )
                    }
                }
            }
            _ => None,
        };
        if let Err(error) = validate_command_delivery(
            intent.trigger_kind,
            schedule
                .as_ref()
                .is_some_and(|schedule| schedule.session_id_template.is_some()),
        ) {
            return reject(
                intent,
                FireResult::Rejected {
                    error: error.to_string(),
                },
            );
        }
        let Some(host) = self.command_host.clone() else {
            return reject(
                intent,
                FireResult::Errored {
                    error: "this trigger engine runs no command Tasks".into(),
                },
            );
        };
        let node_did = snapshot.local_did.clone();
        let key = intent
            .trigger_id
            .clone()
            .unwrap_or_else(|| format!("manual:{}", intent.task.task_id));
        let run_id = uuid::Uuid::new_v4().to_string();
        let cancellation = TaskHookCancellation::under(&self.command_shutdown);
        let (done_tx, done_rx) = watch::channel(false);
        let superseded = {
            let mut runs = self.command_runs.lock().await;
            let now = Utc::now();
            let window = runs.admitted.entry(key.clone()).or_default();
            while window
                .front()
                .is_some_and(|fire| fire.at <= now - chrono::Duration::hours(1))
            {
                window.pop_front();
            }
            if let Some(prior) = window
                .iter()
                .find(|fire| fire.fire_key == intent.durable_fire_key)
            {
                let request_id = prior.run_id.clone();
                drop(runs);
                return reject(intent, FireResult::Duplicate { request_id });
            }
            let admitted = window.len();
            let admission = admit_command(
                intent.concurrency,
                runs.running
                    .get(&key)
                    .is_some_and(|running| !running.is_empty()),
            );
            match admission {
                CommandAdmission::RefuseQueued => {
                    drop(runs);
                    return reject(
                        intent,
                        FireResult::Rejected {
                            error: "queued_serial queues fires for an Agent; a command Task's Trigger takes parallel, serial or latest_only".into(),
                        },
                    );
                }
                CommandAdmission::SkipBusy => {
                    drop(runs);
                    return reject(
                        intent,
                        FireResult::Skipped {
                            reason: SERIAL_BUSY.to_string(),
                        },
                    );
                }
                CommandAdmission::SupersedeThenStart | CommandAdmission::Start => {}
            }
            if let Some(schedule) = &schedule {
                if super::durable::fire_budget_exhausted(schedule.max_fires_per_hour, admitted) {
                    drop(runs);
                    let reason =
                        super::durable::budget_stop_reason(&key, schedule.max_fires_per_hour);
                    if let Err(error) = host.stop_for_budget(&node_did, &key, &reason).await {
                        return reject(
                            intent,
                            FireResult::Errored {
                                error: format!("stop Trigger for its fire budget: {error}"),
                            },
                        );
                    }
                    return reject(intent, FireResult::BudgetExhausted { reason });
                }
            }
            runs.admitted
                .entry(key.clone())
                .or_default()
                .push_back(AdmittedFire {
                    at: now,
                    fire_key: intent.durable_fire_key.clone(),
                    run_id: run_id.clone(),
                });
            let running = runs.running.entry(key.clone()).or_default();
            // Interrupted only once this fire is admitted, so a refused fire
            // leaves the run it would have superseded going.
            let superseded = if admission == CommandAdmission::SupersedeThenStart {
                running
                    .iter()
                    .map(|run| {
                        run.cancellation.interrupt();
                        run.done.clone()
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            running.push(RunningCommand {
                run_id: run_id.clone(),
                cancellation: cancellation.clone(),
                done: done_rx,
            });
            superseded
        };
        tracing::info!(
            task_id = %intent.task.task_id,
            trigger_id = ?intent.trigger_id,
            %run_id,
            "starting command Task run for trigger fire"
        );
        let record = RunRecord {
            host,
            node_did,
            trigger_id: intent.trigger_id.clone(),
        };
        let runs = self.command_runs.clone();
        let task_id = intent.task.task_id.clone();
        let started = run_id.clone();
        tokio::spawn(async move {
            for mut prior in superseded {
                let _ = prior.wait_for(|done| *done).await;
            }
            run_command(record, &task_id, command, &run_id, cancellation).await;
            let mut runs = runs.lock().await;
            if let Some(running) = runs.running.get_mut(&key) {
                running.retain(|run| run.run_id != run_id);
                if running.is_empty() {
                    runs.running.remove(&key);
                }
            }
            drop(runs);
            let _ = done_tx.send(true);
        });
        reject(intent, FireResult::CommandStarted { run_id: started })
    }
}

async fn run_command(
    record: RunRecord,
    task_id: &str,
    command: TaskCommand,
    run_id: &str,
    cancellation: TaskHookCancellation,
) {
    let started_at = now();
    let mut run = CommandRun {
        run_id: run_id.to_owned(),
        state: CommandRunState::Running,
        exit_code: None,
        output_tail: None,
        started_at,
        finished_at: None,
    };
    record.write(&run).await;
    let hook = TaskHook {
        hook_id: "command".into(),
        phase: TaskHookPhase::Before,
        command: command.argv,
        timeout_secs: command.timeout_secs,
        skip_exit_codes: Vec::new(),
    };
    match record.host.command_root().await {
        Ok(cwd) => {
            let attempt = ManagedTaskHookExec::new(cwd, cancellation)
                .attempt(&hook)
                .await;
            run.state = finished_state(&attempt.result);
            run.exit_code = match attempt.result {
                HookCommandResult::Exited { code } => code,
                _ => None,
            };
            let tail = attempt.output_tail();
            run.output_tail = (!tail.is_empty()).then_some(tail);
        }
        Err(error) => {
            run.state = CommandRunState::Failed;
            run.output_tail = Some(format!("{error:#}"));
        }
    }
    run.finished_at = Some(now());
    tracing::info!(%task_id, %run_id, state = ?run.state, exit_code = ?run.exit_code, "command Task run finished");
    record.write(&run).await;
}
