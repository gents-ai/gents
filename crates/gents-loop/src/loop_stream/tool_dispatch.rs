mod effect_batch;

use super::*;
use crate::tool::ToolCallEnvelope;
use crate::tool_call_lifecycle::runtime::{
    CurrentToolRuntimeContext, ToolRequestIdentity, ToolScopeEntry,
};

pub fn tool_effect_dispatcher<H>(
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    hook: Arc<H>,
    parent_internal_id: String,
) -> Arc<dyn crate::tool_effects::ToolEffectDispatcher>
where
    H: SessionHook + 'static,
{
    Arc::new(EffectDispatcher {
        tools,
        hook,
        parent_internal_id,
        fatal: Default::default(),
        composed: Default::default(),
    })
}

pub(super) struct EffectDispatcher<H> {
    pub(super) tools: Arc<Vec<Box<dyn ToolDyn>>>,
    pub(super) hook: Arc<H>,
    pub(super) parent_internal_id: String,
    // Hook termination remains fatal through guest error conversion and parent settlement.
    pub(super) fatal: tokio::sync::Mutex<Option<String>>,
    // Set by the first child call, so an invocation that never composed owes no settlement read.
    pub(super) composed: std::sync::atomic::AtomicBool,
}

impl<H> EffectDispatcher<H>
where
    H: SessionHook + 'static,
{
    pub(super) async fn settle_parent_result(
        &self,
        name: &str,
        call_id: Option<String>,
        arguments: &str,
        outcome: &ToolOutcome,
    ) -> crate::ToolResultHookAction {
        if let Err(reason) =
            crate::tool_effects::ToolEffectDispatcher::settle_stopped(self, outcome).await
        {
            self.fatal.lock().await.get_or_insert(reason);
        }
        let action = self
            .hook
            .on_tool_result(name, call_id, &self.parent_internal_id, arguments, outcome)
            .await;
        match self.fatal.lock().await.as_ref() {
            Some(reason) => crate::ToolResultHookAction::Terminate {
                reason: reason.clone(),
            },
            None => action,
        }
    }
}

impl<H> crate::tool_effects::ToolEffectDispatcher for EffectDispatcher<H>
where
    H: SessionHook + 'static,
{
    fn call<'a>(
        &'a self,
        parent: &'a ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> crate::tool::BoxFuture<'a, crate::tool_effects::EffectResult> {
        Box::pin(async move {
            let call = crate::tool_effects::ToolEffectCall {
                ordinal,
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            };
            match self.effect_batch(parent, vec![call]).await.pop() {
                Some(result) => result,
                None => Err(crate::tool_effects::ToolEffectError::Fatal(
                    "the tool batch did not settle its call".to_owned(),
                )),
            }
        })
    }

    fn batch<'a>(
        &'a self,
        parent: &'a ToolCallEnvelope,
        calls: Vec<crate::tool_effects::ToolEffectCall>,
    ) -> crate::tool::BoxFuture<'a, Vec<crate::tool_effects::EffectResult>> {
        Box::pin(self.effect_batch(parent, calls))
    }

    fn definitions<'a>(&'a self) -> crate::tool::BoxFuture<'a, Vec<crate::tool::ToolDefinition>> {
        Box::pin(async move {
            let mut definitions = Vec::with_capacity(self.tools.len());
            for tool in self.tools.iter() {
                definitions.push(tool.definition(String::new()).await);
            }
            definitions
        })
    }

    fn settle_stopped<'a>(
        &'a self,
        stop: &'a ToolOutcome,
    ) -> crate::tool::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if !self.composed.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            self.hook
                .settle_tool_effects(&self.parent_internal_id, stop)
                .await
        })
    }
}

pub fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

pub(super) fn admit_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: &str,
    context: &ToolCallEnvelope,
) -> Option<ToolOutcome> {
    admit_from(
        tools,
        name,
        args,
        context,
        crate::tool_policy::presentation::Origin::Provider,
    )
}

fn admit_tool_effect(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: &str,
    context: &ToolCallEnvelope,
) -> Option<ToolOutcome> {
    admit_from(
        tools,
        name,
        args,
        context,
        crate::tool_policy::presentation::Origin::Composition,
    )
}

fn admit_from(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: &str,
    context: &ToolCallEnvelope,
    origin: crate::tool_policy::presentation::Origin,
) -> Option<ToolOutcome> {
    if let Err(refusal) = crate::tool_policy::presentation::admit(tools, name, origin) {
        return Some(ToolOutcome::from_dispatch(name, Err(refusal.into())));
    }
    let tool = tools.iter().find(|tool| tool.name() == name)?;
    tool.admit(args, context)
        .err()
        .map(|error| ToolOutcome::from_dispatch(name, Err(error)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReadySettlement {
    TimedOut,
    ReceiptFailure,
    EarlyFailure,
    Completed,
}

pub(super) fn settle_ready(
    expired: bool,
    receipt_failed: bool,
    early_failure: bool,
) -> ReadySettlement {
    if expired {
        ReadySettlement::TimedOut
    } else if receipt_failed {
        ReadySettlement::ReceiptFailure
    } else if early_failure {
        ReadySettlement::EarlyFailure
    } else {
        ReadySettlement::Completed
    }
}

/// The envelope a plugin's child call runs under: the parent's deadline,
/// cancellation and working directory, the parent's identity and extensions,
/// no live output, and no dispatcher of its own (Lean
/// `CallEnvelope.child_has_no_dispatcher`). `None` when the parent has no
/// request execution scope.
pub fn child_envelope(parent: &ToolCallEnvelope) -> Option<ToolCallEnvelope> {
    let scope = parent.runtime()?;
    Some(child_envelope_under(
        parent,
        &scope,
        scope.cancellation_token.clone(),
    ))
}

pub(super) fn child_envelope_under(
    parent: &ToolCallEnvelope,
    scope: &CurrentToolRuntimeContext,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> ToolCallEnvelope {
    let runtime = CurrentToolRuntimeContext::entered_with_session(
        Some(scope),
        None,
        scope.deadline_at,
        cancellation_token,
        scope.workspace_cwd.clone(),
        None,
        scope.session_id.clone(),
    );
    parent
        .clone()
        .with_runtime(Some(runtime))
        .with_effects(None)
}

/// The envelope one dispatched tool runs under inside `context`'s scope.
pub(super) fn dispatch_envelope(
    context: &ToolCallEnvelope,
    scope: &CurrentToolRuntimeContext,
    live_output: Option<crate::live_output::LiveToolOutputWriter>,
    session_id: Option<String>,
) -> ToolCallEnvelope {
    context
        .clone()
        .with_runtime(Some(CurrentToolRuntimeContext::entered_with_session(
            Some(scope),
            None,
            scope.deadline_at,
            scope.cancellation_token.clone(),
            scope.workspace_cwd.clone(),
            live_output,
            session_id.or(scope.session_id.clone()),
        )))
}

/// The envelope a foreground call runs under inside the envelope `hook` gives
/// it, and the call's own cancellation: a child of the hook's. The admitted
/// request `scope` is re-entered under that token, and the loop's dispatcher
/// serves the call's children. Without a scope the call sees the hook's.
pub fn foreground_envelope(
    hook: &ToolCallEnvelope,
    scope: Option<CurrentToolRuntimeContext>,
    effects: Option<Arc<dyn crate::tool_effects::ToolEffectDispatcher>>,
) -> (ToolCallEnvelope, tokio_util::sync::CancellationToken) {
    let hook_token = hook
        .runtime()
        .map(|scope| scope.cancellation_token)
        .unwrap_or_default();
    let token = hook_token.child_token();
    let call = match scope {
        Some(scope) => hook
            .clone()
            .with_runtime(Some(CurrentToolRuntimeContext::entered(
                hook.runtime().as_ref(),
                ToolScopeEntry {
                    identity: Some(ToolRequestIdentity::new(
                        scope.requester_did,
                        scope.node_did,
                        scope.agent_did,
                        scope.request_id,
                    )),
                    deadline_at: scope.deadline_at,
                    cancellation_token: token.clone(),
                    workspace_cwd: scope.workspace_cwd,
                    live_output: scope.live_output,
                    session_id: scope.session_id,
                    correlation: scope.correlation,
                    source_fields: scope.source_fields,
                    background: scope.background,
                },
            ))),
        None => hook.clone(),
    };
    (call.with_effects(effects), token)
}

pub async fn dispatch_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::live_output::LiveToolOutputWriter>,
    session_id: Option<String>,
    context: &ToolCallEnvelope,
) -> ToolOutcome {
    let dispatch = ProviderDispatch {
        live_output,
        session_id,
        token_limit: false,
        cleanup: None,
    };
    dispatch_provider_tool(tools, name, args, dispatch, context).await
}

/// What the loop supplies for one provider call besides its envelope.
pub(super) struct ProviderDispatch {
    pub(super) live_output: Option<crate::live_output::LiveToolOutputWriter>,
    pub(super) session_id: Option<String>,
    pub(super) token_limit: bool,
    pub(super) cleanup: Option<crate::session_hook::NativeCleanupCheck>,
}

const MAX_UNKNOWN_TOOL_SUGGESTIONS: usize = 8;
const MAX_UNKNOWN_TOOL_AVAILABLE: usize = 20;

fn unknown_tool_message(name: &str, tools: &[Box<dyn ToolDyn>]) -> String {
    let mut available: Vec<String> = tools
        .iter()
        .filter(|tool| !tool.hidden())
        .map(|tool| tool.name())
        .collect();
    available.sort();
    available.dedup();
    if available.is_empty() {
        return format!("error: unknown tool '{name}'; no tools are available in this turn");
    }
    let needle = name.to_ascii_lowercase();
    let mut matches: Vec<&str> = if needle.is_empty() {
        Vec::new()
    } else {
        available
            .iter()
            .map(String::as_str)
            .filter(|tool| {
                let lower = tool.to_ascii_lowercase();
                lower.starts_with(&needle)
                    || lower.contains(&needle)
                    || (lower.len() >= 2 && needle.contains(&lower))
            })
            .collect()
    };
    if matches.is_empty() && !needle.is_empty() {
        let tokens: Vec<&str> = needle
            .split(['_', '-', '.', '/'])
            .filter(|token| token.len() >= 3)
            .collect();
        if !tokens.is_empty() {
            matches = available
                .iter()
                .map(String::as_str)
                .filter(|tool| {
                    let lower = tool.to_ascii_lowercase();
                    lower
                        .split(['_', '-', '.', '/'])
                        .any(|segment| tokens.contains(&segment))
                })
                .collect();
        }
    }
    if !matches.is_empty() {
        let total = matches.len();
        matches.truncate(MAX_UNKNOWN_TOOL_SUGGESTIONS);
        let more = total.saturating_sub(matches.len());
        let suffix = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        return format!(
            "error: unknown tool '{name}'; did you mean: {}{suffix}?",
            matches.join(", ")
        );
    }
    let total = available.len();
    let shown: Vec<&str> = available
        .iter()
        .take(MAX_UNKNOWN_TOOL_AVAILABLE)
        .map(String::as_str)
        .collect();
    let more = total.saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    format!(
        "error: unknown tool '{name}'; available tools: {}{suffix}",
        shown.join(", ")
    )
}

pub(super) async fn dispatch_provider_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    dispatch: ProviderDispatch,
    context: &ToolCallEnvelope,
) -> ToolOutcome {
    let ProviderDispatch {
        live_output,
        session_id,
        token_limit,
        cleanup,
    } = dispatch;
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        return ToolOutcome::from_dispatch(
            name,
            Err(crate::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::ArgumentInvalid,
                text: unknown_tool_message(name, tools),
            }),
        );
    };

    let outcome = |result: Result<String, crate::tool::ToolError>| {
        ToolOutcome::from_dispatch(
            name,
            result.map_err(|error| crate::tool::with_provider_token_limit(error, token_limit)),
        )
    };

    let Some(scope) = context.runtime() else {
        return outcome(
            crate::tool_call_lifecycle::runtime::dispatch_with_receipt(
                tool.as_ref(),
                args,
                live_output,
                context,
            )
            .await,
        );
    };
    let tool_call = dispatch_envelope(context, &scope, live_output.clone(), session_id.clone());

    if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
        let decided = ToolOutcome::TimedOut {
            deadline_at: scope.deadline_at,
        };
        if let Some(check) = &cleanup {
            check(Some(&decided));
        }
        return decided;
    }

    let deadline_at = scope.deadline_at;
    let mut deadline = Box::pin(async move {
        match deadline_remaining(deadline_at) {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    });

    let receipt_writer = live_output;
    let mut call = tool.call_with_receipt(args, &tool_call);
    tokio::select! {
        biased;
        _ = scope.cancellation_token.cancelled() => {
            let decided = if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
                ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
            } else { ToolOutcome::Cancelled };
            if cleanup.as_ref().is_some_and(|check| check(Some(&decided))) {
                if let Ok(cleaned) = tokio::time::timeout(crate::tool_call_lifecycle::runtime::NATIVE_PROCESS_CLEANUP_GRACE, &mut call).await {
                    if let (Some(check), Err(error)) = (&cleanup, cleaned.result) { check(Some(&outcome(Err(error)))); }
                }
            }
            ToolOutcome::Cancelled
        },
        _ = &mut deadline => {
            if cleanup.as_ref().is_some_and(|check| check(Some(&ToolOutcome::TimedOut { deadline_at: scope.deadline_at }))) {
                if let Ok(cleaned) = tokio::time::timeout(crate::tool_call_lifecycle::runtime::NATIVE_PROCESS_CLEANUP_GRACE, &mut call).await {
                    if let (Some(check), Err(error)) = (&cleanup, cleaned.result) { check(Some(&outcome(Err(error)))); }
                }
            }
            ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
        },
        mut dispatched = &mut call => {
            let original = std::mem::replace(&mut dispatched.result, Ok(String::new()));
            let (successful, early_failure) = match original {
                Ok(value) => (Some(value), None),
                Err(error) => (None, Some(outcome(Err(error)))),
            };
            if let (Some(check), Some(failure)) = (&cleanup, &early_failure) { check(Some(failure)); }
            let receipt = crate::tool_call_lifecycle::runtime::record_dispatch_receipt(dispatched, receipt_writer).await.err();
            let expired = deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero());
            let decided = match settle_ready(expired, receipt.is_some(), early_failure.is_some()) {
                ReadySettlement::TimedOut => ToolOutcome::TimedOut { deadline_at: scope.deadline_at },
                ReadySettlement::ReceiptFailure => outcome(Err(receipt.expect("receipt failure"))),
                ReadySettlement::EarlyFailure => early_failure.expect("early failure"),
                ReadySettlement::Completed => ToolOutcome::Completed(successful.expect("dispatch result")),
            };
            if let Some(check) = &cleanup { check(Some(&decided)); }
            decided
        },
    }
}

pub(super) struct ForegroundExecution<H> {
    pub(super) tools: Arc<Vec<Box<dyn ToolDyn>>>,
    pub(super) name: String,
    pub(super) args: String,
    pub(super) live_output: Option<crate::live_output::LiveToolOutputWriter>,
    pub(super) session_id: Option<String>,
    pub(super) effects: Option<Arc<EffectDispatcher<H>>>,
    pub(super) scope: Option<crate::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
    pub(super) token_limit: bool,
    pub(super) batch_token: Option<tokio_util::sync::CancellationToken>,
}

pub(super) fn foreground_call<H>(
    input: ForegroundExecution<H>,
) -> crate::session_hook::ForegroundCall
where
    H: SessionHook + 'static,
{
    Box::new(move |hook, cleanup| {
        Box::pin(async move {
            let ForegroundExecution {
                tools,
                name,
                args,
                live_output,
                session_id,
                effects,
                scope,
                token_limit,
                batch_token,
            } = input;
            let effects =
                effects.map(|value| value as Arc<dyn crate::tool_effects::ToolEffectDispatcher>);
            let (call, token) = foreground_envelope(&hook, scope.clone(), effects);
            let execute = async move {
                dispatch_provider_tool(
                    tools.as_slice(),
                    &name,
                    args,
                    ProviderDispatch {
                        live_output,
                        session_id,
                        token_limit,
                        cleanup,
                    },
                    &call,
                )
                .await
            };
            let scoped = async {
                match scope {
                    Some(_) => execute.await,
                    None => tokio::select! {
                        biased;
                        _ = token.cancelled() => ToolOutcome::Cancelled,
                        outcome = execute => outcome,
                    },
                }
            };
            match batch_token {
                Some(batch) => {
                    tokio::pin!(scoped);
                    tokio::select! {
                        biased;
                        _ = batch.cancelled() => { token.cancel(); scoped.await }
                        outcome = &mut scoped => outcome,
                    }
                }
                None => scoped.await,
            }
        })
    })
}

pub(super) async fn settle_foreground<H>(
    hook: &H,
    effects: Option<&Arc<EffectDispatcher<H>>>,
    name: &str,
    call_id: Option<String>,
    internal_id: &str,
    args: &str,
    outcome: &ToolOutcome,
) -> crate::ToolResultHookAction
where
    H: SessionHook + 'static,
{
    match effects {
        Some(effects) => {
            effects
                .settle_parent_result(name, call_id, args, outcome)
                .await
        }
        None => {
            hook.on_tool_result(name, call_id, internal_id, args, outcome)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::doubles::{
        AdmissionGateTool as AdmissionGate, EffectHook, ParseOnlyTool, ReadyAfterWallDeadline,
        StrictArgsTool,
    };
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn generated_argument_failures_follow_streamed_provider_reason() {
        use crate::provider_stream::{guard_response, ProviderStreamProtocol};
        use bytes::Bytes;
        use futures::StreamExt;

        #[derive(serde::Deserialize)]
        struct Case {
            termination: String,
            arguments: String,
            kind: String,
        }
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../../../gents/tests/fixtures/tool_argument_failure.json"
        ))
        .unwrap();
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ParseOnlyTool)];
        for case in cases {
            let payload = match case.termination.as_str() {
                "token_limit" => {
                    "data: {\"choices\":[{\"finish_reason\":\"length\"}]}\r\n\r\ndata: [DONE]\r\n\r\n"
                }
                "completed" => {
                    "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\r\n\r\n"
                }
                "unreported" => "data: [DONE]\r\n\r\n",
                other => panic!("unrecognized generated termination {other}"),
            };
            for split in 0..=payload.len() {
                let activity = Arc::new(crate::provider_activity::ProviderActivity::default());
                let body: rig::http_client::sse::BoxedStream = Box::pin(futures::stream::iter([
                    Ok(Bytes::copy_from_slice(&payload.as_bytes()[..split])),
                    Ok(Bytes::copy_from_slice(&payload.as_bytes()[split..])),
                ]));
                let response = rig::http_client::Response::builder()
                    .status(200)
                    .body(body)
                    .unwrap();
                let chunks = guard_response(
                    response,
                    Some(ProviderStreamProtocol::ChatCompletions),
                    Some(activity.clone()),
                )
                .into_body()
                .collect::<Vec<_>>()
                .await;
                let actual: Vec<u8> = chunks.into_iter().flat_map(Result::unwrap).collect();
                assert_eq!(actual, payload.as_bytes());
                for scoped in [false, true] {
                    let context = if scoped {
                        crate::tool_call_lifecycle::runtime::ToolScopeEntry::new(
                            None,
                            tokio_util::sync::CancellationToken::new(),
                        )
                        .call()
                    } else {
                        crate::tool::ToolCallEnvelope::default()
                    };
                    let outcome = dispatch_provider_tool(
                        &tools,
                        "parse",
                        case.arguments.clone(),
                        ProviderDispatch {
                            live_output: None,
                            session_id: None,
                            token_limit: activity.token_limit_reached(),
                            cleanup: None,
                        },
                        &context,
                    )
                    .await;
                    let ToolOutcome::Failed { class, text, .. } = outcome else {
                        panic!("invalid arguments executed")
                    };
                    assert_eq!(
                        class,
                        crate::tool_call_lifecycle::FailureClass::ArgumentInvalid
                    );
                    assert!(
                        text.contains(&format!("({})", case.kind)),
                        "{} split {split}: {text}",
                        case.termination
                    );
                }
            }
        }
    }

    fn effect_dispatcher(
        tools: Vec<Box<dyn ToolDyn>>,
        hook: Arc<EffectHook>,
    ) -> EffectDispatcher<EffectHook> {
        EffectDispatcher {
            tools: Arc::new(tools),
            hook,
            parent_internal_id: "parent".into(),
            fatal: Default::default(),
            composed: Default::default(),
        }
    }

    fn wall_tool() -> Box<dyn ToolDyn> {
        Box::new(ReadyAfterWallDeadline {
            deadline_at: Utc::now(),
            called: Arc::new(AtomicBool::new(false)),
            fails: false,
        })
    }

    fn scope_call(deadline: Option<chrono::DateTime<Utc>>) -> ToolCallEnvelope {
        ToolScopeEntry::new(deadline, tokio_util::sync::CancellationToken::new()).call()
    }

    #[tokio::test]
    async fn definitions_are_exactly_the_admitted_tool_surface() {
        use crate::tool_effects::ToolEffectDispatcher;
        let hook = Arc::new(EffectHook::default());
        let dispatcher =
            effect_dispatcher(vec![Box::new(ParseOnlyTool), wall_tool()], hook.clone());
        let definitions = dispatcher.definitions().await;
        let names: Vec<_> = definitions.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["parse", "ready_after_wall_deadline"]);
        for (name, args) in [
            ("parse", r#"{"text":"valid"}"#),
            ("ready_after_wall_deadline", "{}"),
        ] {
            assert!(names.contains(&name));
            assert!(admit_tool_effect(
                dispatcher.tools.as_slice(),
                name,
                args,
                &ToolCallEnvelope::default()
            )
            .is_none());
        }
        assert!(admit_tool_effect(
            dispatcher.tools.as_slice(),
            "unlisted",
            "{}",
            &ToolCallEnvelope::default()
        )
        .is_some());
        let outcome = dispatcher
            .call(&scope_call(None), 1, "unlisted", "{}")
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Failed { .. }));
        assert!(hook.rejected.load(Ordering::SeqCst));
        assert!(!hook.invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_hidden_tool_is_refused_to_the_model_and_called_for_a_plugin() {
        use crate::tool_effects::ToolEffectDispatcher;
        let hook = Arc::new(EffectHook::default());
        let tools: Vec<Box<dyn ToolDyn>> = vec![
            Box::new(AdmissionGate {
                name: "script",
                admitted: true,
            }),
            Box::new(ParseOnlyTool),
        ];
        let tools = crate::tool_policy::presentation::present_tools(tools, &["parse".into()]);
        let args = r#"{"text":"valid"}"#;
        let refused = admit_tool(&tools, "parse", args, &ToolCallEnvelope::default()).unwrap();
        let ToolOutcome::Failed { class, text, .. } = refused else {
            panic!("a hidden tool was admitted for the model");
        };
        assert_eq!(
            class,
            crate::tool_call_lifecycle::FailureClass::PolicyDenied
        );
        assert!(text.contains("`script`"), "{text}");
        assert!(admit_tool(&tools, "unlisted", "{}", &ToolCallEnvelope::default()).is_none());
        let dispatcher = effect_dispatcher(tools, hook.clone());
        let names: Vec<_> = dispatcher
            .definitions()
            .await
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert_eq!(names, ["script", "parse"]);
        let outcome = dispatcher
            .call(&scope_call(None), 1, "parse", args)
            .await
            .unwrap();
        assert_eq!(outcome.model_facing_text(), "specialized side effect");
        assert!(hook.invoked.load(Ordering::SeqCst));
        assert!(!hook.rejected.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn unselected_specialized_tool_never_reaches_before_hook() {
        use crate::tool_effects::ToolEffectDispatcher;
        let hook = Arc::new(EffectHook::default());
        let dispatcher = effect_dispatcher(vec![], hook.clone());
        let outcome = dispatcher
            .call(&scope_call(None), 1, "agent_interrupt", "{}")
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Failed { .. }));
        assert!(hook.rejected.load(Ordering::SeqCst));
        assert!(!hook.invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn unknown_tool_suggests_matching_or_available_visible_tools() {
        let tools: Vec<Box<dyn ToolDyn>> = vec![
            Box::new(AdmissionGate {
                name: "config",
                admitted: true,
            }),
            Box::new(AdmissionGate {
                name: "agent_new",
                admitted: true,
            }),
            Box::new(AdmissionGate {
                name: "agent_message",
                admitted: true,
            }),
            Box::new(AdmissionGate {
                name: "agent_list",
                admitted: true,
            }),
            Box::new(AdmissionGate {
                name: "query",
                admitted: true,
            }),
        ];
        let envelope = ToolCallEnvelope::default();

        // Prefix/substring match suggests the related tools.
        let outcome = dispatch_tool(&tools, "agent", "{}".into(), None, None, &envelope).await;
        let ToolOutcome::Failed { class, text, .. } = outcome else {
            panic!("unknown tool must fail");
        };
        assert_eq!(
            class,
            crate::tool_call_lifecycle::FailureClass::ArgumentInvalid
        );
        assert!(text.starts_with("error: unknown tool 'agent'"), "{text}");
        assert!(
            text.contains("agent_list, agent_message, agent_new"),
            "{text}"
        );

        // Token match suggests tools sharing a segment.
        let outcome =
            dispatch_tool(&tools, "agent_create", "{}".into(), None, None, &envelope).await;
        let ToolOutcome::Failed { text, .. } = outcome else {
            panic!("unknown tool must fail");
        };
        assert!(
            text.contains("agent_list, agent_message, agent_new"),
            "{text}"
        );

        // Unmatched tool lists available tools so the model sees bash is off.
        let outcome = dispatch_tool(&tools, "bash", "{}".into(), None, None, &envelope).await;
        let ToolOutcome::Failed { text, .. } = outcome else {
            panic!("unknown tool must fail");
        };
        assert!(text.starts_with("error: unknown tool 'bash'"), "{text}");
        assert!(
            text.contains("available tools: agent_list, agent_message, agent_new, config, query"),
            "{text}"
        );

        // Hidden tools are never leaked in unknown-tool suggestions.
        let presented: Vec<Box<dyn ToolDyn>> = crate::tool_policy::presentation::present_tools(
            vec![
                Box::new(AdmissionGate {
                    name: "script",
                    admitted: true,
                }),
                Box::new(ParseOnlyTool),
            ],
            &["parse".into()],
        );
        let outcome = dispatch_tool(&presented, "pars", "{}".into(), None, None, &envelope).await;
        let ToolOutcome::Failed { text, .. } = outcome else {
            panic!("unknown tool must fail");
        };
        assert!(!text.contains("parse"), "hidden tool leaked: {text}");
        assert!(text.contains("available tools: script"), "{text}");

        // Empty tool list reports none available.
        let empty: Vec<Box<dyn ToolDyn>> = Vec::new();
        let outcome = dispatch_tool(&empty, "bash", "{}".into(), None, None, &envelope).await;
        let ToolOutcome::Failed { text, .. } = outcome else {
            panic!("unknown tool must fail");
        };
        assert!(text.contains("no tools are available"), "{text}");
    }

    #[tokio::test]
    async fn before_hook_effect_outlasting_the_deadline_is_not_interrupted() {
        use crate::tool_effects::ToolEffectDispatcher;
        let hook = Arc::new(EffectHook {
            yield_before: true,
            ..Default::default()
        });
        let dispatcher = effect_dispatcher(vec![wall_tool()], hook.clone());
        let expired = Some(Utc::now() - chrono::Duration::seconds(1));
        let outcome = dispatcher
            .call(&scope_call(expired), 1, "ready_after_wall_deadline", "{}")
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ToolOutcome::Completed("specialized side effect".into())
        );
        assert!(!hook.cleaned.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn child_fatal_stops_outer_continuation_after_parent_settlement() {
        use crate::tool_effects::ToolEffectDispatcher;
        for cleanup_fails in [false, true] {
            let hook = Arc::new(EffectHook {
                cleanup_fails,
                terminate_before: true,
                ..Default::default()
            });
            let dispatcher = effect_dispatcher(vec![wall_tool()], hook.clone());
            let result = dispatcher
                .call(&scope_call(None), 1, "ready_after_wall_deadline", "{}")
                .await;
            assert!(matches!(
                result,
                Err(crate::tool_effects::ToolEffectError::Fatal(_))
            ));
            let outcome = ToolOutcome::Completed("guest swallowed the error".into());
            let action = dispatcher
                .settle_parent_result("plugin", None, "{}", &outcome)
                .await;
            assert!(hook.parent_settled.load(Ordering::SeqCst));
            let crate::ToolResultHookAction::Terminate { reason } = action else {
                panic!("fatal child allowed outer continuation");
            };
            assert_eq!(
                reason,
                if cleanup_fails {
                    "durable cleanup unavailable"
                } else {
                    "child hook terminated"
                }
            );
        }
    }

    #[test]
    fn plugin_child_admission_matches_the_lean_model() {
        gents_lean_vocab::families::plugin_resources_child_routes::replay(|i| {
            let name = if i.static_hook_owned {
                gents_protocol::goal_names::GET_GOAL_TOOL_NAME
            } else {
                "workflow_run"
            };
            let surface = if i.selected { name } else { "unselected" };
            let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(AdmissionGate {
                name: surface,
                admitted: i.admitted,
            })];
            admit_tool_effect(&tools, name, "{}", &ToolCallEnvelope::default()).is_none()
        });
    }

    #[test]
    fn ready_settlement_matches_the_lean_model() {
        gents_lean_vocab::families::tool_dispatch_settle_ready::replay::<ReadySettlement>(|i| {
            settle_ready(i.expired, i.receipt_failed, i.early_failure)
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deadline_dispatch_matches_the_lean_model() {
        for case in gents_lean_vocab::families::tool_dispatch_dispatch_under_deadline::cases::<
            ReadySettlement,
        >() {
            let now = Utc::now();
            let (deadline_at, ready_at) = if case.input.expired_at_dispatch {
                (now - chrono::Duration::seconds(1), now)
            } else if case.input.ready_after_deadline {
                let deadline_at = now + chrono::Duration::milliseconds(300);
                (deadline_at, deadline_at)
            } else {
                (
                    now + chrono::Duration::seconds(60),
                    now - chrono::Duration::seconds(1),
                )
            };
            let called = Arc::new(AtomicBool::new(false));
            let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ReadyAfterWallDeadline {
                deadline_at: ready_at,
                called: Arc::clone(&called),
                fails: case.input.fails,
            })];
            let scope_call = crate::tool_call_lifecycle::runtime::ToolScopeEntry::new(
                Some(deadline_at),
                tokio_util::sync::CancellationToken::new(),
            )
            .call();
            let outcome = dispatch_tool(
                &tools,
                "ready_after_wall_deadline",
                "{}".into(),
                None,
                None,
                &scope_call,
            )
            .await;
            let settlement = match outcome {
                ToolOutcome::TimedOut {
                    deadline_at: Some(recorded),
                } if recorded == deadline_at => ReadySettlement::TimedOut,
                ToolOutcome::Failed {
                    class: crate::tool_call_lifecycle::FailureClass::ToolReturnedError,
                    denial: None,
                    text,
                } if text == "tool failed" => ReadySettlement::EarlyFailure,
                ToolOutcome::Completed(_) => ReadySettlement::Completed,
                other => panic!("{:?}: unexpected outcome {other:?}", case.input),
            };
            case.check(gents_lean_vocab::families::tool_dispatch_dispatch_under_deadline::DeadlineDispatch {
                called: called.load(Ordering::SeqCst),
                settlement,
            });
        }
    }

    #[tokio::test]
    async fn dispatch_tool_types_unparseable_args_as_argument_invalid() {
        #[derive(serde::Deserialize)]
        struct Case {
            termination: String,
            arguments: String,
            kind: String,
        }
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../../../gents/tests/fixtures/tool_argument_failure.json"
        ))
        .unwrap();
        let cases: Vec<_> = cases
            .into_iter()
            .filter(|case| case.termination == "unreported" && case.kind == "malformed")
            .collect();
        assert!(!cases.is_empty());
        let calls = Arc::new(AtomicUsize::new(0));
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool {
            calls: calls.clone(),
        })];
        for case in cases {
            let result = super::dispatch_tool(
                &tools,
                "strict",
                case.arguments,
                None,
                None,
                &crate::tool::ToolCallEnvelope::default(),
            )
            .await;
            match &result {
                crate::tool_call_lifecycle::ToolOutcome::Failed {
                    class,
                    denial: None,
                    text,
                } => {
                    assert_eq!(
                        *class,
                        crate::tool_call_lifecycle::FailureClass::ArgumentInvalid
                    );
                    assert!(
                        text.contains(&format!("arguments were rejected ({})", case.kind)),
                        "{text}"
                    );
                    assert!(
                        !text.contains("ran") && !text.contains("token limit"),
                        "{text}"
                    );
                }
                other => panic!("unparseable args must classify ArgumentInvalid, got {other:?}"),
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }
}
