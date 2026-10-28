use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use gents::background_tools::ProcessControlScope;
use gents::config_client::ConfigAccess;
use gents::plugin_job::control::control;
use gents::plugin_job::dispatch::split_address;
use gents::plugin_job::view::{list_jobs, load_job_view, JobFilter, JobKey, StateFilter};
use gents_protocol::job::{
    JobChildState, JobControlKind, JobControlOutcome, JobOrigin, JobPauseReason, JobPhaseState,
    JobReadError, JobState, JobView,
};

use serde_json::{json, Value};

use crate::cli::args::{
    JobCommand, JobControlArgs, JobListArgs, JobPromoteArgs, JobShowArgs, JobStateArg, JobWatchArgs,
};
use crate::cli::output_format::OutputFormat;
use crate::commands::chat::streaming::sanitize_summary_text;
use crate::home_state::resolve_node_did;
use crate::request_helpers::print_ndjson;
use crate::{print_json, resolve_config_access};

pub(crate) mod promote;

pub(crate) const ACTIVE_STATES: [JobState; 3] =
    [JobState::Running, JobState::Paused, JobState::AwaitingUser];
const ALL_STATES: [JobState; 7] = [
    JobState::Running,
    JobState::Paused,
    JobState::AwaitingUser,
    JobState::Completed,
    JobState::Failed,
    JobState::Cancelled,
    JobState::TimedOut,
];

pub(crate) async fn dispatch(command: JobCommand) -> Result<()> {
    match command {
        JobCommand::List(args) => job_list(args).await,
        JobCommand::Show(args) => job_show(args).await,
        JobCommand::Watch(args) => job_watch(args).await,
        JobCommand::Stop(args) => job_control(args, JobControlKind::Stop).await,
        JobCommand::Pause(args) => job_control(args, JobControlKind::Pause).await,
        JobCommand::Promote(args) => job_promote(args).await,
    }
}

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const NOT_YET_OBSERVABLE_LIMIT: Duration = Duration::from_secs(120);
const NOT_OBSERVABLE_EXIT: i32 = 4;

async fn job_watch(args: JobWatchArgs) -> Result<()> {
    let format = args.output.ensure_supported(
        "job watch",
        &[OutputFormat::Text, OutputFormat::Json, OutputFormat::Ndjson],
    )?;
    let (access, session_id, tool_call_id) = match watch_target(&args).await {
        Ok(target) => target,
        Err(error) => {
            let (message, code) = pre_poll_failure(&error);
            eprintln!("{message}");
            std::process::exit(code);
        }
    };
    let redraw = format == OutputFormat::Text && io::stdout().is_terminal();
    let started = std::time::Instant::now();
    let ended = watch_views(
        || {
            load_job_view(
                &access,
                None,
                JobKey::Address {
                    session_id: session_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                },
            )
        },
        || started.elapsed(),
        |pause| tokio::time::sleep(pause),
        Duration::from_millis(args.interval_ms.max(100)),
        args.until_terminal,
        |view| match format {
            OutputFormat::Json => print_json(view),
            OutputFormat::Ndjson => print_ndjson(&serde_json::to_value(view)?),
            _ => {
                if redraw {
                    print!("\x1b[2J\x1b[H");
                }
                for line in show_lines(view) {
                    println!("{line}");
                }
                Ok(())
            }
        },
    )
    .await?;
    if let (Ok(view), true) = (&ended, args.until_terminal) {
        print_ndjson(&terminal_line(view))?;
    }
    if let Err(error) = &ended {
        eprintln!("{}", failure_text(error, &session_id));
    }
    match exit_code(&ended) {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// The access and address a watch polls; any failure here precedes the first poll.
async fn watch_target(args: &JobWatchArgs) -> Result<(ConfigAccess, String, String)> {
    let (access, _home_dir) =
        resolve_config_access(args.scope.home.as_deref(), args.scope.graphql.as_deref()).await?;
    require_served(&access)?;
    let (session_id, tool_call_id) = match split_address(&args.selector) {
        Some(address) => address,
        None => {
            let actor =
                resolve_node_did(args.scope.home.as_deref(), args.scope.node_did.as_deref())
                    .context("resolving job owner node_did")?;
            let view =
                resolve_job(&access, &actor, &args.selector, args.session.as_deref()).await?;
            (view.session_id, view.tool_call_id)
        }
    };
    Ok((access, session_id, tool_call_id))
}

/// The message and exit code of a watch that cannot start polling: not observable.
fn pre_poll_failure(error: &anyhow::Error) -> (String, i32) {
    (format!("{error:#}"), NOT_OBSERVABLE_EXIT)
}

/// A Local access holds the home store claim, which would block `gents server`.
fn require_served(access: &ConfigAccess) -> Result<()> {
    match access {
        ConfigAccess::Graphql(_) => Ok(()),
        ConfigAccess::Local(_) => {
            anyhow::bail!("no running server for this home; start it, or pass --graphql")
        }
    }
}

/// The exit code a row's state ends a watch with; `None` while it still runs.
/// Decided on `JobView.state`, never `progress.state`.
fn terminal_exit(state: JobState) -> Option<i32> {
    match state {
        JobState::Running => None,
        JobState::Completed => Some(0),
        JobState::Paused | JobState::AwaitingUser => Some(3),
        JobState::Failed | JobState::Cancelled | JobState::TimedOut => Some(1),
    }
}

fn exit_code(ended: &Result<JobView, JobReadError>) -> i32 {
    match ended {
        Ok(view) => terminal_exit(view.state).unwrap_or_default(),
        Err(_) => NOT_OBSERVABLE_EXIT,
    }
}

fn failure_text(error: &JobReadError, session_id: &str) -> String {
    match error {
        JobReadError::NotYetObservable => {
            format!("not found: {error}\nnext: gents job list --session {session_id}")
        }
        error => error.to_string(),
    }
}

fn terminal_line(view: &JobView) -> Value {
    let progress = view.progress.as_ref();
    let pause_reason = progress.and_then(|progress| progress.pause_reason);
    let next = next_calls(view.state, &view.address).into_iter().next();
    let mut line = json!({
        "state": state_name(view.state),
        "address": view.address,
        "handle": view.handle,
        "outcome": view.outcome,
        "spawned": progress.map(|progress| progress.spawned),
        "budget": progress.map(|progress| json!({
            "total": progress.budget.total,
            "spent": progress.budget.spent,
            "remaining": progress.budget.remaining,
        })),
        "next": next,
    });
    if let (JobState::Paused, Some(reason)) = (view.state, pause_reason) {
        line["pause_reason"] = json!(pause_reason_name(reason));
    }
    line
}

/// Polls until the row is terminal and returns it, or the reader error that
/// ends the watch. `Unavailable` backs off (1 s doubling, capped 30 s) and never
/// ends it; `NotYetObservable` backs off the same way for 120 s from its first
/// poll, then ends it. `on_change` sees each changed view, and is not called
/// with `until_terminal`.
async fn watch_views<Poll, PollFut, Sleep, SleepFut>(
    mut poll: Poll,
    mut now: impl FnMut() -> Duration,
    mut sleep: Sleep,
    interval: Duration,
    until_terminal: bool,
    mut on_change: impl FnMut(&JobView) -> Result<()>,
) -> Result<Result<JobView, JobReadError>>
where
    Poll: FnMut() -> PollFut,
    PollFut: Future<Output = Result<JobView, JobReadError>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let mut last: Option<JobView> = None;
    let mut backoff = BACKOFF_START;
    let mut unseen_since: Option<Duration> = None;
    loop {
        match poll().await {
            Ok(view) => {
                backoff = BACKOFF_START;
                unseen_since = None;
                if !until_terminal && last.as_ref() != Some(&view) {
                    on_change(&view)?;
                }
                if terminal_exit(view.state).is_some() {
                    return Ok(Ok(view));
                }
                last = Some(view);
                sleep(interval).await;
            }
            Err(error) if !error.is_transient() => return Ok(Err(error)),
            Err(error) => {
                if error == JobReadError::NotYetObservable {
                    let since = *unseen_since.get_or_insert_with(&mut now);
                    if now().saturating_sub(since) >= NOT_YET_OBSERVABLE_LIMIT {
                        return Ok(Err(error));
                    }
                }
                sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
        }
    }
}

async fn job_show(args: JobShowArgs) -> Result<()> {
    let format = args
        .output
        .ensure_supported("job show", &[OutputFormat::Text, OutputFormat::Json])?;
    let actor = resolve_node_did(args.scope.home.as_deref(), args.scope.node_did.as_deref())
        .context("resolving job owner node_did")?;
    let (access, _home_dir) =
        resolve_config_access(args.scope.home.as_deref(), args.scope.graphql.as_deref()).await?;
    let view = resolve_job(&access, &actor, &args.selector, args.session.as_deref()).await?;
    match format {
        OutputFormat::Json => print_json(&view),
        _ => {
            for line in show_lines(&view) {
                println!("{line}");
            }
            Ok(())
        }
    }
}

pub(crate) async fn job_promote(args: JobPromoteArgs) -> Result<()> {
    let format = args
        .output
        .ensure_supported("job promote", &[OutputFormat::Text, OutputFormat::Json])?;
    let actor = resolve_node_did(args.scope.home.as_deref(), args.scope.node_did.as_deref())
        .context("resolving job owner node_did")?;
    let (access, home) =
        resolve_config_access(args.scope.home.as_deref(), args.scope.graphql.as_deref()).await?;
    let text = format == OutputFormat::Text;
    let result = promote::promote(
        &access,
        &home,
        &actor,
        &args.selector,
        args.name.as_deref(),
        args.agent.as_deref(),
        &mut |step| {
            if text {
                println!("{}", step.name());
            }
        },
    )
    .await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    let report = result.map_err(|error| {
        let code = error.refusal_code();
        let error = anyhow::Error::new(error);
        match code {
            Some(code) => error.context(code),
            None => error,
        }
    })?;
    match format {
        OutputFormat::Json => print_json(&report),
        _ => {
            println!(
                "promoted: {}; call it with args and agent_budget",
                report.plugin
            );
            Ok(())
        }
    }
}

async fn job_control(args: JobControlArgs, kind: JobControlKind) -> Result<()> {
    let actor = resolve_node_did(args.scope.home.as_deref(), args.scope.node_did.as_deref())
        .context("resolving job owner node_did")?;
    let (access, _home_dir) =
        resolve_config_access(args.scope.home.as_deref(), args.scope.graphql.as_deref()).await?;
    let view = resolve_job(&access, &actor, &args.selector, args.session.as_deref()).await?;
    for line in control_job(&access, &view, kind).await? {
        println!("{line}");
    }
    Ok(())
}

/// The operator acts as the job's own scope; the owner refuses a GraphQL access.
pub(crate) fn operator_scope(view: &JobView) -> ProcessControlScope {
    ProcessControlScope {
        request_id: String::new(),
        session_id: view.scope.session_id.clone(),
        node_did: view.scope.node_did.clone(),
        requester_did: view.scope.requester_did.clone(),
    }
}

async fn control_job(
    access: &ConfigAccess,
    view: &JobView,
    kind: JobControlKind,
) -> Result<Vec<String>> {
    let result = control(access, &operator_scope(view), &view.tool_call_id, kind).await;
    control_lines(kind, view, result)
}

/// Answer first. A job that is not ours or not a job is an error carrying the
/// same lines; any other reader error prints as its `Display`.
pub(crate) fn control_lines(
    kind: JobControlKind,
    view: &JobView,
    result: Result<JobControlOutcome, JobReadError>,
) -> Result<Vec<String>> {
    let verb = match kind {
        JobControlKind::Stop => "stop",
        JobControlKind::Pause => "pause",
    };
    match result {
        Ok(JobControlOutcome::Applied {
            state,
            killed_children,
        }) => {
            let mut lines = vec![format!(
                "{}  {verb} applied  {killed_children} children interrupted",
                state_name(state)
            )];
            lines.push("next:".to_owned());
            for call in next_calls(state, &view.address) {
                lines.push(format!("  {call}"));
            }
            Ok(lines)
        }
        Ok(JobControlOutcome::AlreadyTerminal { state }) => Ok(vec![
            "workflow_control_not_applicable".to_owned(),
            format!("state: {}", state_name(state)),
            "next:".to_owned(),
            format!("  gents job show {}", view.address),
        ]),
        Err(JobReadError::NotAJob | JobReadError::NotAuthorized) => Err(anyhow::anyhow!(
            "workflow_control_unknown_run\nnext:\n  gents job list"
        )),
        Err(error) => Err(read_error(error)),
    }
}

async fn job_list(args: JobListArgs) -> Result<()> {
    let format = args
        .output
        .ensure_supported("job list", &[OutputFormat::Table, OutputFormat::Json])?;
    let (access, _home_dir) =
        resolve_config_access(args.scope.home.as_deref(), args.scope.graphql.as_deref()).await?;
    let views = list_jobs(
        &access,
        &JobFilter {
            session_id: args.session,
            requester: None,
            state: state_filter(&args.state),
            plugin: args.plugin,
        },
    )
    .await
    .map_err(read_error)?;
    match format {
        OutputFormat::Json => print_json(&views),
        _ => {
            print_table(&views, Utc::now().timestamp_millis().max(0) as u64);
            Ok(())
        }
    }
}

fn state_filter(args: &[JobStateArg]) -> StateFilter {
    let mut states = Vec::new();
    for arg in args {
        let named = match arg {
            JobStateArg::Running => vec![JobState::Running],
            JobStateArg::Paused => vec![JobState::Paused],
            JobStateArg::AwaitingUser => vec![JobState::AwaitingUser],
            JobStateArg::Completed => vec![JobState::Completed],
            JobStateArg::Failed => vec![JobState::Failed],
            JobStateArg::Cancelled => vec![JobState::Cancelled],
            JobStateArg::TimedOut => vec![JobState::TimedOut],
            JobStateArg::All => ALL_STATES.to_vec(),
        };
        for state in named {
            if !states.contains(&state) {
                states.push(state);
            }
        }
    }
    if states.is_empty() {
        states = ACTIVE_STATES.to_vec();
    }
    StateFilter::States(states)
}

/// A reader error as the command's error: its `Display` is the message, and an
/// unobservable row is "not found" with no retry.
pub(crate) fn read_error(error: JobReadError) -> anyhow::Error {
    match error {
        JobReadError::NotYetObservable => anyhow::anyhow!("not found: {error}"),
        error => anyhow::Error::new(error),
    }
}

pub(crate) async fn resolve_job(
    access: &ConfigAccess,
    actor: &str,
    selector: &str,
    session: Option<&str>,
) -> Result<JobView> {
    if let Some((session_id, tool_call_id)) = split_address(selector) {
        return load_job_view(
            access,
            None,
            JobKey::Address {
                session_id,
                tool_call_id,
            },
        )
        .await
        .map_err(read_error);
    }
    let views = list_jobs(
        access,
        &JobFilter {
            session_id: session.map(Into::into),
            requester: None,
            state: StateFilter::States(ALL_STATES.to_vec()),
            plugin: None,
        },
    )
    .await
    .map_err(read_error)?;
    pick_job(&views, actor, selector, session).cloned()
}

/// An address, else a bare `tool_call_id` among the actor's rows, else a handle.
pub(crate) fn pick_job<'a>(
    views: &'a [JobView],
    actor: &str,
    selector: &str,
    session: Option<&str>,
) -> Result<&'a JobView> {
    if split_address(selector).is_some() {
        return views
            .iter()
            .find(|view| view.address == selector)
            .ok_or_else(|| not_found(selector));
    }
    let by_call: Vec<&JobView> = views
        .iter()
        .filter(|view| view.tool_call_id == selector && view.scope.node_did == actor)
        .collect();
    match by_call.as_slice() {
        [only] => return Ok(only),
        [] => {}
        _ => return Err(ambiguous(selector, &by_call)),
    }
    let named = views
        .iter()
        .filter(|view| view.handle == selector && session.is_none_or(|id| view.session_id == id));
    let candidates: Vec<&JobView> = if session.is_some() {
        named.collect()
    } else {
        let mut heads: BTreeMap<&str, &JobView> = BTreeMap::new();
        for view in named {
            let head = heads.entry(&view.pin.chain_root_doc_id).or_insert(view);
            if view.adopted_at > head.adopted_at {
                *head = view;
            }
        }
        heads
            .into_values()
            .filter(|view| ACTIVE_STATES.contains(&view.state))
            .collect()
    };
    let Some(latest) = candidates.iter().max_by_key(|view| view.adopted_at) else {
        return Err(not_found(selector));
    };
    if candidates
        .iter()
        .any(|view| view.session_id != latest.session_id)
    {
        return Err(ambiguous(selector, &candidates));
    }
    Ok(latest)
}

fn not_found(selector: &str) -> anyhow::Error {
    anyhow::anyhow!("not found: no job matches `{selector}`; list jobs with `gents job list`")
}

fn ambiguous(selector: &str, candidates: &[&JobView]) -> anyhow::Error {
    let calls: Vec<String> = candidates
        .iter()
        .map(|view| format!("  gents job show {}", view.address))
        .collect();
    anyhow::anyhow!(
        "`{selector}` matches more than one job; show one with:\n{}",
        calls.join("\n")
    )
}

/// The `gents job` calls that follow a job in `state`. How a paused or failed
/// run is resumed is the plugin's own contract (its TOOL.md), not the host's.
pub fn next_calls(state: JobState, address: &str) -> Vec<String> {
    match state {
        JobState::Running => vec![
            format!("gents job watch {address}"),
            format!("gents job stop {address}"),
        ],
        JobState::Paused | JobState::AwaitingUser => vec![
            format!("gents job show {address}"),
            format!("gents job stop {address}"),
        ],
        JobState::Completed | JobState::Failed | JobState::Cancelled | JobState::TimedOut => {
            vec![format!("gents job show {address}")]
        }
    }
}

/// Guest-sourced text as one printable line, through the shared output filter.
pub(crate) fn clean(text: &str) -> String {
    sanitize_summary_text(text, usize::MAX).unwrap_or_default()
}

/// Answer first: state and outcome, then phases, children, question, next calls
/// and metadata. A section whose source is an absent `progress` is omitted.
fn show_lines(view: &JobView) -> Vec<String> {
    let progress = view.progress.as_ref();
    let title = progress.map_or_else(|| clean(&view.handle), |progress| clean(&progress.title));
    let mut lines = vec![format!("{}  {title}", state_name(view.state))];
    if let Some(outcome) = &view.outcome {
        lines.push(format!(
            "outcome: {}  {}",
            clean(&outcome.code),
            clean(&outcome.message)
        ));
    }
    let pause_reason = progress.and_then(|progress| progress.pause_reason);
    if let (JobState::Paused, Some(reason)) = (view.state, pause_reason) {
        lines.push(format!("pause_reason: {}", pause_reason_name(reason)));
    }
    if let Some(progress) = progress {
        if !progress.phases.is_empty() {
            lines.push("phases:".to_owned());
            for phase in &progress.phases {
                lines.push(format!(
                    "  {}  {}",
                    phase_state_name(phase.state),
                    clean(&phase.title)
                ));
            }
        }
    }
    if !view.children.is_empty() {
        lines.push("children:".to_owned());
        for child in &view.children {
            lines.push(format!(
                "  {}  attempt {}  {}  {}  {} tokens  {}",
                clean(&child.slot),
                child.attempt,
                clean(&child.title),
                child_state_name(child.state),
                child.usage.total,
                age(child.duration_ms)
            ));
        }
    }
    if let (JobState::AwaitingUser, Some(question)) = (
        view.state,
        progress.and_then(|progress| progress.question.as_ref()),
    ) {
        lines.push(format!("question: {}", clean(question)));
    }
    lines.push("next:".to_owned());
    for call in next_calls(view.state, &view.address) {
        lines.push(format!("  {call}"));
    }
    lines.push("metadata:".to_owned());
    lines.push(format!("  script_id: {}", clean(&view.pin.script_id)));
    lines.push(format!("  origin: {}", origin_name(view.pin.origin)));
    lines.push(format!("  engine_digest: {}", view.pin.engine_digest));
    lines.push(format!("  plugin: {}", view.pin.plugin_ref));
    lines.push(format!("  cwd: {}", view.pin.cwd));
    lines.push(format!("  created_at: {}", view.created_at));
    lines.push(format!("  adopted_at: {}", view.adopted_at));
    if let Some(completed_at) = view.completed_at {
        lines.push(format!("  completed_at: {completed_at}"));
    }
    lines
}

fn pause_reason_name(reason: JobPauseReason) -> &'static str {
    match reason {
        JobPauseReason::User => "user",
        JobPauseReason::BackOff => "back_off",
        JobPauseReason::NoProgress => "no_progress",
        JobPauseReason::Verification => "verification",
        JobPauseReason::Infra => "infra",
        JobPauseReason::BudgetExceeded => "budget_exceeded",
    }
}

fn phase_state_name(state: JobPhaseState) -> &'static str {
    match state {
        JobPhaseState::Pending => "pending",
        JobPhaseState::Active => "active",
        JobPhaseState::Done => "done",
        JobPhaseState::Failed => "failed",
    }
}

fn child_state_name(state: JobChildState) -> &'static str {
    match state {
        JobChildState::Live => "live",
        JobChildState::Completed => "completed",
        JobChildState::Failed => "failed",
        JobChildState::Interrupted => "interrupted",
        JobChildState::Other => "other",
    }
}

fn origin_name(origin: JobOrigin) -> &'static str {
    match origin {
        JobOrigin::Installed => "installed",
        JobOrigin::Dynamic => "dynamic",
    }
}

const HEADERS: [&str; 9] = [
    "HANDLE", "PLUGIN", "STATE", "PHASE", "SPAWNED", "LIVE", "AGE", "SESSION", "ADDRESS",
];

/// A column whose source is in an absent `progress` is `-`, never 0.
fn row_cells(view: &JobView, now_ms: u64) -> [String; 9] {
    let progress = view.progress.as_ref();
    let phase = progress
        .and_then(|progress| {
            progress
                .phases
                .iter()
                .find(|phase| phase.state == JobPhaseState::Active)
        })
        .map(|phase| clean(&phase.title));
    [
        clean(&view.handle),
        view.pin.plugin_ref.clone(),
        state_name(view.state).to_owned(),
        phase.unwrap_or_else(|| "-".to_owned()),
        progress.map_or_else(
            || "-".to_owned(),
            |progress| format!("{}/{}", progress.spawned, progress.budget.total),
        ),
        progress.map_or_else(
            || "-".to_owned(),
            |progress| progress.active.len().to_string(),
        ),
        age(now_ms.saturating_sub(view.created_at)),
        view.session_id.clone(),
        view.address.clone(),
    ]
}

pub(crate) fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::AwaitingUser => "awaiting_user",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
        JobState::TimedOut => "timed_out",
    }
}

fn age(ms: u64) -> String {
    let secs = ms / 1_000;
    let (days, hours, minutes) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m{}s", secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn print_table(views: &[JobView], now_ms: u64) {
    let mut rows = vec![HEADERS.map(str::to_owned)];
    rows.extend(views.iter().map(|view| row_cells(view, now_ms)));
    let widths: Vec<usize> = (0..HEADERS.len())
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::plugin_job::admit::Refusal;
    use gents::plugin_job::dispatch::split_address;
    use gents_protocol::job::{
        JobBudget, JobChildRow, JobChildState, JobOutcome, JobPauseReason, JobPhase, JobPhaseState,
        JobPinSummary, JobProgress, JobScope, JobState, JobUsage, JobView,
    };

    const ACTOR: &str = "did:key:actor";

    fn view(session: &str, call: &str, handle: &str, state: JobState, adopted_at: u64) -> JobView {
        JobView {
            address: format!("{session}:{call}"),
            row_doc_id: format!("row-{session}-{call}"),
            session_id: session.to_owned(),
            tool_call_id: call.to_owned(),
            scope: JobScope {
                session_id: session.to_owned(),
                request_id: "request".to_owned(),
                node_did: ACTOR.to_owned(),
                requester_did: None,
            },
            handle: handle.to_owned(),
            state,
            outcome: None,
            pin: JobPinSummary {
                script_id: "script".to_owned(),
                origin: gents_protocol::job::JobOrigin::Installed,
                engine_digest: "digest".to_owned(),
                plugin_ref: "ns/name".to_owned(),
                agent_budget: 4,
                cwd: "/work".to_owned(),
                chain_root_doc_id: format!("root-{handle}"),
                resume_of: None,
            },
            progress: None,
            children: Vec::new(),
            created_at: adopted_at,
            adopted_at,
            completed_at: None,
        }
    }

    fn progress() -> JobProgress {
        JobProgress {
            title: "title".to_owned(),
            state: JobState::Running,
            pause_reason: None,
            phases: vec![
                JobPhase {
                    title: "plan".to_owned(),
                    state: JobPhaseState::Done,
                },
                JobPhase {
                    title: "build".to_owned(),
                    state: JobPhaseState::Active,
                },
            ],
            active: Vec::new(),
            spawned: 2,
            budget: JobBudget {
                total: 5,
                spent: 2,
                remaining: 3,
            },
            last_event: String::new(),
            last_event_at: None,
            question: None,
            observed_terminal_children: 0,
            revision: 0,
        }
    }

    #[test]
    fn address_splits_at_the_last_colon() {
        let views = [
            view("job-session:abc", "call-1", "alpha", JobState::Running, 1),
            view("job-session:abc", "call-2", "beta", JobState::Running, 2),
        ];
        assert_eq!(
            split_address("job-session:abc:call-2"),
            Some(("job-session:abc".to_owned(), "call-2".to_owned()))
        );
        let picked = pick_job(&views, ACTOR, "job-session:abc:call-2", None).unwrap();
        assert_eq!(picked.handle, "beta");
    }

    #[test]
    fn bare_tool_call_id_filters_on_node_did_and_tool_call_id() {
        let mut other = view("s1", "call-1", "other", JobState::Running, 1);
        other.scope.node_did = "did:key:other".to_owned();
        let views = [other, view("s2", "call-1", "mine", JobState::Running, 2)];
        let picked = pick_job(&views, ACTOR, "call-1", None).unwrap();
        assert_eq!(picked.handle, "mine");
    }

    #[test]
    fn ambiguous_selector_lists_show_calls() {
        let views = [
            view("s1", "call-1", "one", JobState::Running, 1),
            view("s2", "call-1", "two", JobState::Running, 2),
        ];
        let error = pick_job(&views, ACTOR, "call-1", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("gents job show s1:call-1"), "{error}");
        assert!(error.contains("gents job show s2:call-1"), "{error}");
    }

    #[test]
    fn handle_with_session_resolves_latest_chain_row() {
        let mut resumed = view("s1", "call-2", "alpha", JobState::Completed, 20);
        resumed.pin.chain_root_doc_id = "root-alpha".to_owned();
        let views = [
            view("s1", "call-1", "alpha", JobState::Failed, 10),
            resumed,
            view("s2", "call-3", "alpha", JobState::Running, 30),
        ];
        let picked = pick_job(&views, ACTOR, "alpha", Some("s1")).unwrap();
        assert_eq!(picked.address, "s1:call-2");
    }

    #[test]
    fn state_filter_parses_the_job_vocabulary() {
        use crate::cli::args::JobStateArg;
        use clap::ValueEnum;
        let cases = [
            ("running", JobState::Running),
            ("paused", JobState::Paused),
            ("awaiting_user", JobState::AwaitingUser),
            ("completed", JobState::Completed),
            ("failed", JobState::Failed),
            ("cancelled", JobState::Cancelled),
            ("timed_out", JobState::TimedOut),
        ];
        for (name, state) in cases {
            let arg = JobStateArg::from_str(name, false).unwrap();
            assert_eq!(state_filter(&[arg]), StateFilter::States(vec![state]));
        }
        let all = JobStateArg::from_str("all", false).unwrap();
        let StateFilter::States(states) = state_filter(&[all]) else {
            panic!("all must map to StateFilter::States");
        };
        assert_eq!(states.len(), 7);
        assert_eq!(
            state_filter(&[]),
            StateFilter::States(vec![
                JobState::Running,
                JobState::Paused,
                JobState::AwaitingUser
            ])
        );
    }

    #[test]
    fn not_yet_observable_reports_not_found_with_job_list() {
        let error = read_error(JobReadError::NotYetObservable).to_string();
        assert!(error.starts_with("not found: "), "{error}");
        assert!(error.contains("gents job list"), "{error}");
        assert_eq!(
            read_error(JobReadError::NotAuthorized).to_string(),
            JobReadError::NotAuthorized.to_string()
        );
    }

    #[test]
    fn list_renders_absent_progress_as_absent() {
        let absent = row_cells(&view("s1", "call-1", "alpha", JobState::Running, 0), 1_000);
        assert_eq!(&absent[3..6], ["-", "-", "-"]);
        let mut with_progress = view("s1", "call-1", "alpha", JobState::Running, 0);
        with_progress.progress = Some(progress());
        let present = row_cells(&with_progress, 1_000);
        assert_eq!(&present[3..6], ["build", "2/5", "0"]);
    }

    #[test]
    fn json_output_names_address_not_key() {
        let value =
            serde_json::to_value([view("s1", "call-1", "alpha", JobState::Running, 1)]).unwrap();
        assert_eq!(value[0]["address"], "s1:call-1");
        assert!(value[0].get("key").is_none());
    }

    fn shown(view: &JobView) -> String {
        show_lines(view).join("\n")
    }

    fn outcome(refusal: Refusal) -> Option<JobOutcome> {
        Some(JobOutcome {
            code: refusal.code().unwrap().to_owned(),
            message: "the engine said so".to_owned(),
        })
    }

    #[test]
    fn show_completed_names_workflow_result() {
        assert_eq!(
            next_calls(JobState::Completed, "s1:call-1"),
            ["gents job show s1:call-1"]
        );
    }

    #[test]
    fn show_step_failed_names_show() {
        assert_eq!(
            next_calls(JobState::Failed, "s1:call-1"),
            ["gents job show s1:call-1"]
        );
        let mut failed = view("s1", "call-1", "alpha", JobState::Failed, 1);
        failed.outcome = outcome(Refusal::StepFailed);
        let text = shown(&failed);
        assert!(text.contains("plugin_job_step_failed"), "{text}");
    }

    #[test]
    fn show_paused_names_show_and_stop() {
        let mut paused = view("s1", "call-1", "alpha", JobState::Paused, 1);
        let mut p = progress();
        p.pause_reason = Some(JobPauseReason::BudgetExceeded);
        paused.progress = Some(p);
        let text = shown(&paused);
        assert!(text.contains("budget_exceeded"), "{text}");
        assert_eq!(
            next_calls(JobState::Paused, "s1:call-1"),
            ["gents job show s1:call-1", "gents job stop s1:call-1"]
        );
    }

    #[test]
    fn show_awaiting_user_prints_the_question() {
        let mut waiting = view("s1", "call-1", "alpha", JobState::AwaitingUser, 1);
        let mut p = progress();
        p.question = Some("Which branch?".to_owned());
        waiting.progress = Some(p);
        let text = shown(&waiting);
        assert!(text.contains("Which branch?"), "{text}");
        assert!(text.contains("gents job show s1:call-1"), "{text}");
    }

    #[test]
    fn show_cancelled_without_outcome_prints_the_state_only() {
        let cancelled = view("s1", "call-1", "alpha", JobState::Cancelled, 1);
        let text = shown(&cancelled);
        assert!(text.contains("cancelled"), "{text}");
        assert!(!text.contains("outcome"), "{text}");
        assert_eq!(
            next_calls(JobState::Cancelled, "s1:call-1"),
            ["gents job show s1:call-1"]
        );
    }

    #[test]
    fn show_children_print_snake_case_state_and_usage_total() {
        let mut parent = view("s1", "call-1", "alpha", JobState::Running, 1);
        parent.children = vec![JobChildRow {
            slot: "slot-a".to_owned(),
            attempt: 2,
            session_id: "child".to_owned(),
            request_id: "req".to_owned(),
            state: JobChildState::Interrupted,
            title: "review".to_owned(),
            usage: JobUsage {
                input: 10,
                output: 5,
                total: 4242,
            },
            duration_ms: 3_000,
            target: "agent-a".to_owned(),
        }];
        let text = shown(&parent);
        assert!(text.contains("interrupted"), "{text}");
        assert!(text.contains("4242"), "{text}");
    }

    #[test]
    fn show_prints_answer_first() {
        let mut waiting = view("s1", "call-1", "alpha", JobState::AwaitingUser, 1);
        let mut p = progress();
        p.question = Some("Which branch?".to_owned());
        waiting.progress = Some(p);
        waiting.outcome = outcome(Refusal::StepFailed);
        waiting.children = vec![JobChildRow {
            slot: "slot-a".to_owned(),
            attempt: 1,
            session_id: "child".to_owned(),
            request_id: "req".to_owned(),
            state: JobChildState::Live,
            title: "review".to_owned(),
            usage: JobUsage {
                input: 1,
                output: 1,
                total: 2,
            },
            duration_ms: 1,
            target: "agent-a".to_owned(),
        }];
        let text = shown(&waiting);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {text}"))
        };
        let order = [
            at("awaiting_user"),
            at("plugin_job_step_failed"),
            at("plan"),
            at("slot-a"),
            at("Which branch?"),
            at("gents job show"),
            at("script"),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    }

    #[test]
    fn next_calls_takes_pure_inputs_through_the_jobs_facade() {
        assert_eq!(
            crate::jobs::next_calls(JobState::Completed, "s:t"),
            ["gents job show s:t"]
        );
        assert_eq!(
            next_calls(JobState::Running, "s:t"),
            ["gents job watch s:t", "gents job stop s:t"]
        );
    }

    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::time::Duration;

    type Ended = Result<JobView, JobReadError>;

    struct Run {
        ended: Ended,
        sleeps: Vec<Duration>,
        clock: Duration,
        changes: usize,
    }

    async fn run(script: Vec<Ended>, fallback: Ended, until_terminal: bool) -> Run {
        let script = RefCell::new(VecDeque::from(script));
        let clock = Cell::new(Duration::ZERO);
        let sleeps = RefCell::new(Vec::new());
        let changes = Cell::new(0);
        let ended = watch_views(
            || {
                let next = script.borrow_mut().pop_front().unwrap_or(fallback.clone());
                std::future::ready(next)
            },
            || clock.get(),
            |pause| {
                clock.set(clock.get() + pause);
                sleeps.borrow_mut().push(pause);
                std::future::ready(())
            },
            Duration::from_secs(1),
            until_terminal,
            |_| {
                changes.set(changes.get() + 1);
                Ok(())
            },
        )
        .await
        .unwrap();
        Run {
            ended,
            sleeps: sleeps.into_inner(),
            clock: clock.get(),
            changes: changes.get(),
        }
    }

    fn secs(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_secs).collect()
    }

    #[tokio::test]
    async fn until_terminal_exit_codes() {
        let cases = [
            (JobState::Completed, 0),
            (JobState::Paused, 3),
            (JobState::AwaitingUser, 3),
            (JobState::Failed, 1),
            (JobState::Cancelled, 1),
            (JobState::TimedOut, 1),
        ];
        for (state, code) in cases {
            let row = view("s1", "call-1", "alpha", state, 1);
            let ended = run(Vec::new(), Ok(row), true).await.ended;
            assert_eq!(exit_code(&ended), code, "{state:?}");
        }
        let not_observable = run(Vec::new(), Err(JobReadError::NotYetObservable), true).await;
        assert_eq!(exit_code(&not_observable.ended), 4);
        let permanent = run(Vec::new(), Err(JobReadError::NotAJob), true).await;
        assert_eq!(exit_code(&permanent.ended), 4);
    }

    #[test]
    fn until_terminal_prints_one_line_with_address() {
        let mut paused = view("s1", "call-1", "alpha", JobState::Paused, 1);
        let mut p = progress();
        p.pause_reason = Some(JobPauseReason::BudgetExceeded);
        paused.progress = Some(p);
        let line = terminal_line(&paused);
        assert_eq!(line["address"], "s1:call-1");
        assert!(line.get("key").is_none());
        assert_eq!(line["state"], "paused");
        assert_eq!(line["handle"], "alpha");
        assert_eq!(line["pause_reason"], "budget_exceeded");
        assert_eq!(line["spawned"], 2);
        assert_eq!(
            line["budget"],
            serde_json::json!({"total": 5, "spent": 2, "remaining": 3})
        );
        assert_eq!(line["next"], "gents job show s1:call-1");

        let cancelled = terminal_line(&view("s1", "call-1", "alpha", JobState::Cancelled, 1));
        assert!(cancelled["outcome"].is_null());
        assert!(cancelled["spawned"].is_null());
        assert!(cancelled["budget"].is_null());
        assert!(cancelled.get("pause_reason").is_none());
        assert_eq!(cancelled["next"], "gents job show s1:call-1");

        let mut done = view("s1", "call-1", "alpha", JobState::Completed, 1);
        done.outcome = outcome(Refusal::StepFailed);
        let line = terminal_line(&done);
        assert_eq!(line["outcome"]["code"], "plugin_job_step_failed");
        assert_eq!(line["next"], "gents job show s1:call-1");
    }

    #[tokio::test]
    async fn until_terminal_prints_nothing_before_terminal() {
        let running = view("s1", "call-1", "alpha", JobState::Running, 1);
        let done = view("s1", "call-1", "alpha", JobState::Completed, 1);
        let quiet = run(
            vec![Ok(running.clone()), Ok(running.clone())],
            Ok(done.clone()),
            true,
        )
        .await;
        assert_eq!(quiet.changes, 0);
        let streamed = run(vec![Ok(running.clone()), Ok(running)], Ok(done), false).await;
        assert_eq!(streamed.changes, 2);
    }

    #[tokio::test]
    async fn unavailable_backs_off_then_succeeds() {
        let unavailable = || {
            Err(JobReadError::Unavailable {
                message: "down".to_owned(),
            })
        };
        let done = view("s1", "call-1", "alpha", JobState::Completed, 1);
        let script = (0..7).map(|_| unavailable()).collect();
        let run = run(script, Ok(done), true).await;
        assert!(run.ended.is_ok());
        assert_eq!(run.sleeps, secs(&[1, 2, 4, 8, 16, 30, 30]));
    }

    #[tokio::test]
    async fn not_yet_observable_exits_4_after_120s() {
        let run = run(Vec::new(), Err(JobReadError::NotYetObservable), true).await;
        assert_eq!(run.ended, Err(JobReadError::NotYetObservable));
        assert_eq!(exit_code(&run.ended), 4);
        assert!(run.clock >= Duration::from_secs(120), "{:?}", run.clock);
        assert!(run.clock < Duration::from_secs(150), "{:?}", run.clock);
        let text = failure_text(&JobReadError::NotYetObservable, "s1");
        assert!(text.starts_with("not found: "), "{text}");
        assert!(text.contains("next: gents job list --session s1"), "{text}");
    }

    #[tokio::test]
    async fn permanent_error_exits_4_at_once() {
        let run = run(Vec::new(), Err(JobReadError::NotAJob), true).await;
        assert_eq!(run.ended, Err(JobReadError::NotAJob));
        assert_eq!(exit_code(&run.ended), 4);
        assert!(run.sleeps.is_empty());
        assert_eq!(
            failure_text(&JobReadError::NotAJob, "s1"),
            JobReadError::NotAJob.to_string()
        );
    }

    #[tokio::test]
    async fn watch_refuses_local_access() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        let error = require_served(&ConfigAccess::local(node))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no running server for this home"), "{error}");
        assert!(error.contains("--graphql"), "{error}");
    }

    #[test]
    fn job_text_output_has_no_control_characters() {
        let dirty = "a\x1b[2Jb\x07c\rd";
        let mut row = view("s1", "call-1", dirty, JobState::Failed, 1);
        row.pin.script_id = dirty.to_owned();
        let untitled = show_lines(&row).join("\n");
        assert!(untitled.contains("a[2Jbc d"), "{untitled}");
        row.state = JobState::AwaitingUser;
        let mut progress = progress();
        progress.title = dirty.to_owned();
        progress.phases[1].title = dirty.to_owned();
        progress.question = Some(dirty.to_owned());
        row.progress = Some(progress);
        row.outcome = Some(JobOutcome {
            code: dirty.to_owned(),
            message: dirty.to_owned(),
        });
        let text = show_lines(&row)
            .into_iter()
            .chain(row_cells(&row, 10))
            .chain([untitled])
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("a[2Jbc d"), "{text}");
        assert!(
            !text.chars().any(|c| c.is_control() && c != '\n'),
            "{text:?}"
        );
    }

    #[test]
    fn pre_poll_failure_exits_4_with_the_message() {
        let error = anyhow::anyhow!("no running server for this home; start it, or pass --graphql");
        let (message, code) = pre_poll_failure(&error);
        assert_eq!(code, 4);
        assert!(message.contains("--graphql"), "{message}");
        let (message, code) = pre_poll_failure(&not_found("alpha"));
        assert_eq!(code, 4);
        assert!(message.starts_with("not found:"), "{message}");
    }

    #[tokio::test]
    async fn already_terminal_row_exits_at_once() {
        let failed = view("s1", "call-1", "alpha", JobState::Failed, 1);
        let run = run(Vec::new(), Ok(failed), false).await;
        assert!(run.sleeps.is_empty());
        assert_eq!(run.changes, 1);
        assert_eq!(exit_code(&run.ended), 1);
    }

    #[test]
    fn operator_scope_copies_the_job_scope() {
        let mut row = view("s1", "call-1", "alpha", JobState::Running, 1);
        row.scope.requester_did = Some("did:key:requester".to_owned());
        let scope = operator_scope(&row);
        assert_eq!(scope.request_id, "");
        assert_eq!(scope.session_id, "s1");
        assert_eq!(scope.node_did, ACTOR);
        assert_eq!(scope.requester_did.as_deref(), Some("did:key:requester"));
    }

    #[tokio::test]
    async fn graphql_access_prints_the_owner_refusal() {
        let row = view("s1", "call-1", "alpha", JobState::Running, 1);
        let access = ConfigAccess::graphql("http://127.0.0.1:9/graphql");
        let owner = control(
            &access,
            &operator_scope(&row),
            "call-1",
            JobControlKind::Stop,
        )
        .await
        .unwrap_err();
        assert!(matches!(owner, JobReadError::Unavailable { .. }));
        let printed = control_job(&access, &row, JobControlKind::Stop)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(printed, owner.to_string());
    }

    #[test]
    fn applied_renders_state_and_children_interrupted() {
        let row = view("s1", "call-1", "alpha", JobState::Running, 1);
        let lines = control_lines(
            JobControlKind::Stop,
            &row,
            Ok(JobControlOutcome::Applied {
                state: JobState::Cancelled,
                killed_children: 2,
            }),
        )
        .unwrap();
        assert_eq!(lines[0], "cancelled  stop applied  2 children interrupted");
        assert!(
            lines.contains(&"  gents job show s1:call-1".to_owned()),
            "{lines:?}"
        );
    }

    #[test]
    fn pause_outcome_renders_answer_first() {
        let row = view("s1", "call-1", "alpha", JobState::Running, 1);
        let lines = control_lines(
            JobControlKind::Pause,
            &row,
            Ok(JobControlOutcome::Applied {
                state: JobState::Completed,
                killed_children: 0,
            }),
        )
        .unwrap();
        assert_eq!(lines[0], "completed  pause applied  0 children interrupted");
        assert_eq!(lines[1], "next:");
    }

    #[test]
    fn already_terminal_renders_not_applicable() {
        let row = view("s1", "call-1", "alpha", JobState::Failed, 1);
        let lines = control_lines(
            JobControlKind::Stop,
            &row,
            Ok(JobControlOutcome::AlreadyTerminal {
                state: JobState::Failed,
            }),
        )
        .unwrap();
        assert_eq!(lines[0], "workflow_control_not_applicable");
        assert!(lines.contains(&"state: failed".to_owned()), "{lines:?}");
        assert_eq!(lines.last().unwrap(), "  gents job show s1:call-1");
    }

    #[test]
    fn not_a_job_renders_unknown_run() {
        let row = view("s1", "call-1", "alpha", JobState::Running, 1);
        for error in [JobReadError::NotAJob, JobReadError::NotAuthorized] {
            let text = control_lines(JobControlKind::Stop, &row, Err(error))
                .unwrap_err()
                .to_string();
            assert!(text.starts_with("workflow_control_unknown_run"), "{text}");
            assert!(text.ends_with("gents job list"), "{text}");
        }
    }

    #[test]
    fn missing_row_reports_not_found_with_job_list() {
        let row = view("s1", "call-1", "alpha", JobState::Running, 1);
        let text = control_lines(
            JobControlKind::Pause,
            &row,
            Err(JobReadError::NotYetObservable),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            text,
            format!("not found: {}", JobReadError::NotYetObservable)
        );
        assert!(text.contains("gents job list"), "{text}");
    }
}
