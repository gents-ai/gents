//! Lean `Triggers.ManualRun`: firing one Task now, for every client.

use gents_documents::{read_desired_state_record_in_txn, Agent, Task, TriggerSource};
use gents_protocol::operations::task::{
    ScheduleRunDecision, TaskMatches, TaskOperationError, TaskPresence, TaskRunDecision,
    TaskRunInput, TaskTitleKind,
};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;
use gents_tools::task_run::{RunnableTask, TaskRunOptions, TaskRunOutput};

type Result<T, E = TaskOperationError> = std::result::Result<T, E>;

/// Lean `Triggers.ManualRun.runDecision`.
pub fn run_decision(task: TaskPresence, agent: TaskPresence) -> TaskRunDecision {
    match (task, agent) {
        (TaskPresence::Missing, _) => TaskRunDecision::TaskMissing,
        (TaskPresence::Disabled, _) => TaskRunDecision::TaskDisabled,
        (TaskPresence::Enabled, TaskPresence::Missing) => TaskRunDecision::AgentMissing,
        (TaskPresence::Enabled, TaskPresence::Disabled) => TaskRunDecision::AgentDisabled,
        (TaskPresence::Enabled, TaskPresence::Enabled) => TaskRunDecision::Run,
    }
}

/// Lean `Triggers.ManualRun.scheduleDecision`.
pub fn schedule_decision(
    schedules: TaskMatches,
    enabled_triggers: TaskMatches,
) -> ScheduleRunDecision {
    match (schedules, enabled_triggers) {
        (TaskMatches::None, _) => ScheduleRunDecision::ScheduleMissing,
        (TaskMatches::Many, _) => ScheduleRunDecision::ScheduleAmbiguous,
        (TaskMatches::One, TaskMatches::None) => ScheduleRunDecision::NoEnabledTrigger,
        (TaskMatches::One, TaskMatches::Many) => ScheduleRunDecision::ManyEnabledTriggers,
        (TaskMatches::One, TaskMatches::One) => ScheduleRunDecision::Run,
    }
}

/// Lean `Triggers.ManualRun.sessionTitle`.
pub fn session_title_kind(continues_session: bool, goal_backed: bool) -> TaskTitleKind {
    if continues_session {
        TaskTitleKind::Unchanged
    } else if goal_backed {
        TaskTitleKind::Goal
    } else {
        TaskTitleKind::Run
    }
}

/// Loads the Task for firing: it and the Agent that answers it are enabled
/// under `node_did`, read as whoever `access` acts for.
pub async fn runnable(
    access: &ConfigAccess,
    node_did: &str,
    task_id: &str,
) -> Result<RunnableTask> {
    let (task, agent) = access
        .transact_write("task_run.read", |txn| {
            Box::pin(async move {
                let Some((doc_id, value)) =
                    read_desired_state_record_in_txn(txn, Collection::Task, node_did, task_id)
                        .await?
                else {
                    return Ok::<_, anyhow::Error>((None, None));
                };
                let task: Task = serde_json::from_value(value)?;
                let agent = read_desired_state_record_in_txn(
                    txn,
                    Collection::Agent,
                    node_did,
                    &task.agent_did,
                )
                .await?
                .map(|(_, value)| serde_json::from_value::<Agent>(value))
                .transpose()?;
                Ok((Some((doc_id, task)), agent))
            })
        })
        .await
        .map_err(passthrough)?;
    let presence = |enabled: Option<bool>| match enabled {
        None => TaskPresence::Missing,
        Some(false) => TaskPresence::Disabled,
        Some(true) => TaskPresence::Enabled,
    };
    let decision = run_decision(
        presence(task.as_ref().map(|(_, task)| task.enabled)),
        presence(agent.as_ref().map(|agent| agent.enabled)),
    );
    match (decision, task) {
        (TaskRunDecision::Run, Some((doc_id, task))) => Ok(RunnableTask { task, doc_id }),
        (TaskRunDecision::TaskDisabled, Some((_, task))) => {
            Err(TaskOperationError::TaskDisabled(task.task_id))
        }
        (TaskRunDecision::AgentDisabled, Some((_, task))) => {
            Err(TaskOperationError::AgentDisabled(task.agent_did))
        }
        (TaskRunDecision::AgentMissing, Some((_, task))) => Err(TaskOperationError::AgentMissing {
            agent_did: task.agent_did,
            node_did: node_did.to_owned(),
        }),
        _ => Err(TaskOperationError::TaskMissing {
            task_id: task_id.to_owned(),
            node_did: node_did.to_owned(),
        }),
    }
}

/// Fires `runnable` once, now, signed as `signer`, which must be the actor
/// `access` acts for, and admits it through the Task delivery owner.
pub async fn fire(
    access: &ConfigAccess,
    node_did: &str,
    signer: &dyn NodeIdentity,
    runnable: RunnableTask,
    options: &TaskRunOptions,
) -> Result<TaskRunOutput> {
    if !options.args.is_object() {
        return Err(TaskOperationError::ArgsNotObject(options.args.clone()));
    }
    let goal_backed = runnable.task.goal_objective_template.is_some();
    if goal_backed && options.session_id.is_none() {
        return Err(TaskOperationError::StableKeyRequired(
            runnable.task.task_id.clone(),
        ));
    }
    if !goal_backed && options.session_id.is_some() && options.continue_session.is_some() {
        return Err(TaskOperationError::SessionLabelAndContinue);
    }
    crate::triggers::task_run::enqueue_task_run(access, node_did, signer, runnable, options)
        .await
        .map_err(passthrough)
}

pub async fn run(
    access: &ConfigAccess,
    node_did: &str,
    signer: &dyn NodeIdentity,
    input: &TaskRunInput,
) -> Result<TaskRunOutput> {
    let task = runnable(access, node_did, &input.task_id).await?;
    fire(
        access,
        node_did,
        signer,
        task,
        &TaskRunOptions {
            args: input.args.clone(),
            session_id: input.session_id.clone(),
            continue_session: input.continue_session.clone(),
        },
    )
    .await
}

/// The Task that running `schedule_id` now runs: the Task of its one enabled
/// Trigger under `node_did`.
pub async fn scheduled_task(
    access: &ConfigAccess,
    node_did: &str,
    schedule_id: &str,
) -> Result<String> {
    use gents_protocol::document_query::{fields, rows, Filter, Query, QuerySet, Selection};
    let schedules = Query::select(Selection::doc_id())
        .filter(
            Filter::eq(fields::schedule::NODE_DID, node_did)
                .and(Filter::eq(fields::schedule::SCHEDULE_ID, schedule_id)),
        )
        .limit(2);
    let triggers = Query::select(
        Selection::doc_id()
            .field(fields::trigger::TASK_ID)
            .field(fields::trigger::SOURCE),
    )
    .filter(
        Filter::eq(fields::trigger::NODE_DID, node_did)
            .and(Filter::eq(fields::trigger::ENABLED, true)),
    );
    let response = access
        .read(&QuerySet::new().plain(&schedules).plain(&triggers).render())
        .await
        .map_err(passthrough)?;
    let mut tasks = rows(&response, "Trigger").iter().filter_map(|trigger| {
        let source = serde_json::from_value::<TriggerSource>(trigger["source"].clone()).ok()?;
        matches!(source, TriggerSource::Schedule { schedule_id: id } if id == schedule_id)
            .then(|| trigger["task_id"].as_str().map(ToOwned::to_owned))
            .flatten()
    });
    let (task, another) = (tasks.next(), tasks.next());
    let enabled_triggers =
        TaskMatches::of(usize::from(task.is_some()) + usize::from(another.is_some()));
    let decision = schedule_decision(
        TaskMatches::of(rows(&response, "Schedule").len()),
        enabled_triggers,
    );
    match (decision, task) {
        (ScheduleRunDecision::Run, Some(task)) => Ok(task),
        (ScheduleRunDecision::ScheduleAmbiguous, _) => Err(TaskOperationError::ScheduleAmbiguous {
            schedule_id: schedule_id.to_owned(),
            node_did: node_did.to_owned(),
        }),
        (ScheduleRunDecision::NoEnabledTrigger, _) => {
            Err(TaskOperationError::NoEnabledTrigger(schedule_id.to_owned()))
        }
        (ScheduleRunDecision::ManyEnabledTriggers, _) => Err(
            TaskOperationError::ManyEnabledTriggers(schedule_id.to_owned()),
        ),
        _ => Err(TaskOperationError::ScheduleMissing {
            schedule_id: schedule_id.to_owned(),
            node_did: node_did.to_owned(),
        }),
    }
}

fn passthrough<E>(error: E) -> TaskOperationError
where
    E: Into<anyhow::Error>,
{
    TaskOperationError::Passthrough(
        error
            .into()
            .reallocate_into_boxed_dyn_error_without_backtrace(),
    )
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn run_decision_matches_lean() {
        gents_lean_vocab::families::triggers_manual_run_run_decision::replay(|i| {
            run_decision(i.task, i.agent)
        });
    }

    #[test]
    fn schedule_decision_matches_lean() {
        gents_lean_vocab::families::triggers_manual_run_schedule_decision::replay(|i| {
            schedule_decision(i.schedules, i.enabled_triggers)
        });
    }

    #[test]
    fn session_title_matches_lean() {
        gents_lean_vocab::families::triggers_manual_run_session_title::replay(|i| {
            session_title_kind(i.continues_session, i.goal_backed)
        });
    }
}
