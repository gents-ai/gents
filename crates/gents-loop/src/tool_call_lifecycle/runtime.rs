use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::tool::{ToolDyn, ToolError, UnparseableArgsKind};
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::FailureClass;
use crate::live_output::LiveToolOutputWriter;
use crate::tool_policy::CommandPolicyDenial;

/// Allows an owned native process to finish its bounded termination and reap before its future is dropped.
pub(crate) const NATIVE_PROCESS_CLEANUP_GRACE: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Completed(String),
    Failed {
        class: FailureClass,
        denial: Option<CommandPolicyDenial>,
        text: String,
    },
    TimedOut {
        deadline_at: Option<DateTime<Utc>>,
    },
    Cancelled,
}

impl ToolOutcome {
    pub fn from_dispatch(name: &str, outcome: Result<String, ToolError>) -> Self {
        match outcome {
            Ok(result) => Self::Completed(result),
            Err(ToolError::UnparseableArgs { kind, reason }) => {
                tracing::warn!(
                    tool = name,
                    %kind,
                    %reason,
                    "tool-call arguments unparseable after repair; notifying model"
                );
                let guidance = match kind {
                    UnparseableArgsKind::Truncated => {
                        "the arguments were cut off — your response hit the token limit before \
                         the JSON was complete; re-call the tool with a shorter, complete \
                         arguments object"
                    }
                    UnparseableArgsKind::Malformed => {
                        "the arguments were not valid JSON; re-call the tool with one complete JSON object"
                    }
                    UnparseableArgsKind::UnknownField => {
                        "the JSON contains an unknown field; remove or rename it using the tool's accepted parameter names"
                    }
                    UnparseableArgsKind::MissingField => {
                        "the JSON is missing a required field; add the required field shown below"
                    }
                    UnparseableArgsKind::WrongType => {
                        "a field has the wrong JSON type; use the expected type shown below. Arrays and objects must be native JSON values, not strings containing JSON"
                    }
                    UnparseableArgsKind::Schema => {
                        "the JSON does not match the tool's argument schema; correct the field described below"
                    }
                };
                Self::Failed {
                    class: FailureClass::ArgumentInvalid,
                    denial: None,
                    text: format!(
                        "tool '{name}' arguments were rejected ({kind}): {guidance}. Detail: {reason}"
                    ),
                }
            }
            Err(ToolError::JsonError(error)) => Self::Failed {
                class: FailureClass::ArgumentInvalid,
                denial: None,
                text: error.to_string(),
            },
            Err(ToolError::ReportedFailure { class, text }) => Self::Failed {
                class,
                denial: None,
                text,
            },
            Err(ToolError::ToolCallError(error)) => Self::from_tool_call_error(&error.to_string()),
        }
    }

    pub fn from_tool_call_error(detail: &str) -> Self {
        if let Some(denial) = parse_command_policy_denial(detail) {
            return Self::Failed {
                class: FailureClass::PolicyDenied,
                denial: Some(denial),
                text: detail.to_string(),
            };
        }
        Self::Failed {
            class: classify_error_text(detail),
            denial: None,
            text: detail.to_string(),
        }
    }

    /// A failure the host itself declares, which no tool classified.
    pub fn tool_returned_error(text: &str) -> Self {
        Self::Failed {
            class: FailureClass::ToolReturnedError,
            denial: None,
            text: text.to_string(),
        }
    }

    pub fn model_facing_text(&self) -> &str {
        match self {
            Self::Completed(text) | Self::Failed { text, .. } => text,
            Self::TimedOut { .. } | Self::Cancelled => "",
        }
    }
}

/// The failure class of a tool error known only by its rendering.
///
/// External premise: Rig's `ToolSetError::ToolCallError` erases a tool's error
/// to `Box<dyn Error>`, and MCP servers (through rmcp), plugins and host tools
/// that do not report a class reach the loop only as that text. Tools that
/// know their class return `ToolError::ReportedFailure` instead.
fn classify_error_text(err: &str) -> FailureClass {
    if err.contains("timeout") || err.contains("deadline") {
        FailureClass::External
    } else if err.contains("invalid argument") || err.contains("parse") {
        FailureClass::ArgumentInvalid
    } else if err.contains("unavailable") || err.contains("not found") {
        FailureClass::ServiceUnavailable
    } else if err.contains("transport") || err.contains("connection") {
        FailureClass::Transport
    } else {
        FailureClass::ToolReturnedError
    }
}

fn parse_command_policy_denial(detail: &str) -> Option<CommandPolicyDenial> {
    let payload = strip_error_prefixes(detail);
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value
        .get("failure_class")
        .and_then(serde_json::Value::as_str)
        != Some("policyDenied")
    {
        return None;
    }
    CommandPolicyDenial::from_payload_value(&value)
}

fn strip_error_prefixes(mut value: &str) -> &str {
    loop {
        let stripped = value
            .strip_prefix("error:")
            .or_else(|| value.strip_prefix("Error:"))
            .or_else(|| value.strip_prefix("ERROR:"));
        let Some(stripped) = stripped else {
            return value.trim();
        };
        value = stripped.trim();
    }
}

#[derive(Clone)]
struct ToolRuntimeScope {
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    session_id: Option<String>,
    live_output: Option<LiveToolOutputWriter>,
    background: bool,
    correlation: Option<String>,
    source_fields: std::collections::BTreeMap<String, String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    agent_id: Option<String>,
    request_id: Option<String>,
}

#[derive(Clone)]
pub struct CurrentToolRuntimeContext {
    pub deadline_at: Option<DateTime<Utc>>,
    pub cancellation_token: CancellationToken,
    pub workspace_cwd: Option<PathBuf>,
    pub session_id: Option<String>,
    pub live_output: Option<LiveToolOutputWriter>,
    pub background: bool,
    pub correlation: Option<String>,
    pub source_fields: std::collections::BTreeMap<String, String>,
    pub requester_did: Option<String>,
    pub node_did: Option<String>,
    pub agent_id: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Clone, Default)]
struct ToolRequestIdentityScope {
    requester_did: Option<String>,
    node_did: Option<String>,
    agent_id: Option<String>,
    request_id: Option<String>,
}

tokio::task_local! {
    static TOOL_RUNTIME_SCOPE: ToolRuntimeScope;
    static TOOL_REQUEST_IDENTITY: ToolRequestIdentityScope;
}

fn normalized_identity(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn current_request_identity() -> ToolRequestIdentityScope {
    TOOL_REQUEST_IDENTITY
        .try_with(Clone::clone)
        .ok()
        .or_else(|| {
            TOOL_RUNTIME_SCOPE
                .try_with(|scope| ToolRequestIdentityScope {
                    requester_did: scope.requester_did.clone(),
                    node_did: scope.node_did.clone(),
                    agent_id: scope.agent_id.clone(),
                    request_id: scope.request_id.clone(),
                })
                .ok()
        })
        .unwrap_or_default()
}

pub async fn scope_tool_request_identity<F, T>(
    requester_did: Option<String>,
    node_did: Option<String>,
    agent_id: Option<String>,
    request_id: Option<String>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    TOOL_REQUEST_IDENTITY
        .scope(
            ToolRequestIdentityScope {
                requester_did: normalized_identity(requester_did),
                node_did: normalized_identity(node_did),
                agent_id: normalized_identity(agent_id),
                request_id: normalized_identity(request_id),
            },
            future,
        )
        .await
}

#[cfg(test)]
pub async fn scope_request_tool_execution<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let workspace_cwd = current_tool_runtime_context().and_then(|scope| scope.workspace_cwd);
    scope_request_tool_execution_with_workspace(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        future,
    )
    .await
}

#[cfg(test)]
pub async fn scope_request_tool_execution_with_workspace<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    scope_request_tool_execution_with_session(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        None,
        current_tool_runtime_context().and_then(|scope| scope.session_id),
        future,
    )
    .await
}

pub async fn scope_request_tool_execution_with_session<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    live_output: Option<LiveToolOutputWriter>,
    session_id: Option<String>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let inherited = current_tool_runtime_context();
    let identity = current_request_identity();
    TOOL_RUNTIME_SCOPE
        .scope(
            ToolRuntimeScope {
                deadline_at,
                cancellation_token,
                workspace_cwd,
                session_id,
                live_output,
                background: false,
                correlation: inherited
                    .as_ref()
                    .and_then(|scope| scope.correlation.clone()),
                source_fields: inherited
                    .map(|scope| scope.source_fields)
                    .unwrap_or_default(),
                requester_did: identity.requester_did,
                node_did: identity.node_did,
                agent_id: identity.agent_id,
                request_id: identity.request_id,
            },
            future,
        )
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn scope_request_tool_execution_with_trigger_context<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    live_output: Option<LiveToolOutputWriter>,
    session_id: Option<String>,
    correlation: Option<String>,
    source_fields: std::collections::BTreeMap<String, String>,
    background: bool,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let workspace_cwd = workspace_cwd.or_else(|| {
        TOOL_RUNTIME_SCOPE
            .try_with(|scope| scope.workspace_cwd.clone())
            .ok()
            .flatten()
    });
    let identity = current_request_identity();
    TOOL_RUNTIME_SCOPE
        .scope(
            ToolRuntimeScope {
                deadline_at,
                cancellation_token,
                workspace_cwd,
                session_id,
                live_output,
                background,
                correlation,
                source_fields,
                requester_did: identity.requester_did,
                node_did: identity.node_did,
                agent_id: identity.agent_id,
                request_id: identity.request_id,
            },
            future,
        )
        .await
}

#[cfg(test)]
pub async fn scope_background_tool_execution<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    live_output: Option<LiveToolOutputWriter>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let inherited = current_tool_runtime_context();
    scope_request_tool_execution_with_trigger_context(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        live_output,
        inherited
            .as_ref()
            .and_then(|scope| scope.session_id.clone()),
        inherited
            .as_ref()
            .and_then(|scope| scope.correlation.clone()),
        inherited
            .map(|scope| scope.source_fields)
            .unwrap_or_default(),
        true,
        future,
    )
    .await
}

pub fn invocation_correlation<'a>(
    request: Option<&'a str>,
    supplied: Option<&'a str>,
) -> Option<&'a str> {
    supplied
        .filter(|value| !value.trim().is_empty())
        .or_else(|| request.filter(|value| !value.trim().is_empty()))
}

pub fn current_tool_runtime_context() -> Option<CurrentToolRuntimeContext> {
    TOOL_RUNTIME_SCOPE
        .try_with(Clone::clone)
        .ok()
        .map(|scope| CurrentToolRuntimeContext {
            deadline_at: scope.deadline_at,
            cancellation_token: scope.cancellation_token,
            workspace_cwd: scope.workspace_cwd,
            session_id: scope.session_id,
            live_output: scope.live_output,
            background: scope.background,
            correlation: invocation_correlation(
                scope.request_id.as_deref(),
                scope.correlation.as_deref(),
            )
            .map(str::to_owned),
            source_fields: scope.source_fields,
            requester_did: scope.requester_did,
            node_did: scope.node_did,
            agent_id: scope.agent_id,
            request_id: scope.request_id,
        })
}

pub struct ToolExecutionBounds {
    /// Monotonic origin sampled before the wall clock used to derive
    /// `deadline_at`. The clocks do not share a source, so this only bounds
    /// their offset at arming, not drift afterwards.
    pub deadline_at: Option<DateTime<Utc>>,
    pub cancellation_token: CancellationToken,
    pub live_output: Option<LiveToolOutputWriter>,
    pub armed_at: Instant,
    pub request_deadline_at: Option<DateTime<Utc>>,
}

pub fn tool_execution_bounds(command_timeout: Duration) -> ToolExecutionBounds {
    let runtime = current_tool_runtime_context();
    let request_deadline = runtime.as_ref().and_then(|runtime| runtime.deadline_at);
    let armed_at = Instant::now();
    let command_deadline = Utc::now()
        + chrono::Duration::from_std(command_timeout)
            .unwrap_or_else(|_| chrono::Duration::days(36_500));
    let deadline_at =
        Some(request_deadline.map_or(command_deadline, |deadline| deadline.min(command_deadline)));
    let cancellation_token = runtime
        .as_ref()
        .map(|runtime| runtime.cancellation_token.clone())
        .unwrap_or_default();
    let live_output = runtime.and_then(|runtime| runtime.live_output);
    ToolExecutionBounds {
        deadline_at,
        cancellation_token,
        live_output,
        armed_at,
        request_deadline_at: request_deadline,
    }
}

pub(crate) async fn dispatch_with_receipt(
    tool: &dyn ToolDyn,
    args: String,
    live_output: Option<LiveToolOutputWriter>,
) -> Result<String, ToolError> {
    record_dispatch_receipt(tool.call_with_receipt(args).await, live_output).await
}

pub(crate) async fn record_dispatch_receipt(
    dispatched: crate::tool::ToolDispatchResult,
    live_output: Option<LiveToolOutputWriter>,
) -> Result<String, ToolError> {
    if let Some(writer) = live_output {
        if let Some(receipt) = dispatched.plugin_receipt {
            writer
                .record_plugin_receipt(receipt)
                .await
                .map_err(|error| ToolError::ReportedFailure {
                    class: FailureClass::External,
                    text: format!("plugin execution receipt rejected: {error}"),
                })?;
        }
        if let Some(writes) = dispatched.deferred_writes {
            writer
                .record_deferred_writes(writes)
                .await
                .map_err(|error| ToolError::ReportedFailure {
                    class: FailureClass::External,
                    text: format!("deferred tool writes rejected: {error}"),
                })?;
        }
    }
    dispatched.result
}

/// Cancellation and deadline are polled before the tool result. The result
/// branch also rechecks the wall clock because an inner managed boundary can
/// wake at the same deadline before Tokio observes the sibling sleep as ready.
pub async fn call_tool_managed(tool: &dyn ToolDyn, args: String) -> ToolOutcome {
    call_tool_managed_with_cleanup(tool, args, |_| false).await
}

/// A caller that owns native process cleanup can allow an already spawned
/// process to observe cancellation and publish its stop evidence before the
/// tool future is dropped. A pre-spawn wait is still cancelled immediately.
pub async fn call_tool_managed_with_cleanup<F>(
    tool: &dyn ToolDyn,
    args: String,
    spawned: F,
) -> ToolOutcome
where
    F: Fn(Option<&ToolOutcome>) -> bool,
{
    let name = tool.name();
    let Ok(scope) = TOOL_RUNTIME_SCOPE.try_with(Clone::clone) else {
        return ToolOutcome::from_dispatch(&name, dispatch_with_receipt(tool, args, None).await);
    };

    if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
        let outcome = ToolOutcome::TimedOut {
            deadline_at: scope.deadline_at,
        };
        spawned(Some(&outcome));
        return outcome;
    }

    let deadline_at = scope.deadline_at;
    let mut deadline = Box::pin(async move {
        match deadline_remaining(deadline_at) {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    });

    let mut dispatched = Box::pin(tool.call_with_receipt(args));
    tokio::select! {
        biased;
        _ = scope.cancellation_token.cancelled() => {
            let decided = if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
                ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
            } else { ToolOutcome::Cancelled };
            if spawned(Some(&decided)) {
                if let Ok(cleaned) = tokio::time::timeout(NATIVE_PROCESS_CLEANUP_GRACE, &mut dispatched).await {
                    if let Err(error) = cleaned.result { spawned(Some(&ToolOutcome::from_dispatch(&name, Err(error)))); }
                }
            }
            ToolOutcome::Cancelled
        },
        _ = &mut deadline => {
            if spawned(Some(&ToolOutcome::TimedOut { deadline_at: scope.deadline_at })) {
                if let Ok(cleaned) = tokio::time::timeout(NATIVE_PROCESS_CLEANUP_GRACE, &mut dispatched).await {
                    if let Err(error) = cleaned.result { spawned(Some(&ToolOutcome::from_dispatch(&name, Err(error)))); }
                }
            }
            ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
        },
        mut dispatched = &mut dispatched => {
            let original = std::mem::replace(&mut dispatched.result, Ok(String::new()));
            let (successful, early_failure) = match original {
                Ok(value) => (Some(value), None),
                Err(error) => (None, Some(ToolOutcome::from_dispatch(&name, Err(error)))),
            };
            if let Some(failure) = &early_failure { spawned(Some(failure)); }
            let receipt = record_dispatch_receipt(dispatched, scope.live_output.clone()).await;
            let outcome = if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
                ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
            } else if let Err(error) = receipt {
                ToolOutcome::from_dispatch(&name, Err(error))
            } else if let Some(failure) = early_failure { failure }
            else { ToolOutcome::Completed(successful.expect("dispatch result")) };
            spawned(Some(&outcome));
            outcome
        },
    }
}

pub fn deadline_remaining(deadline_at: Option<DateTime<Utc>>) -> Option<Duration> {
    let deadline_at = deadline_at?;
    let now = Utc::now();
    if now >= deadline_at {
        return Some(Duration::ZERO);
    }
    Some((deadline_at - now).to_std().unwrap_or(Duration::ZERO))
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
