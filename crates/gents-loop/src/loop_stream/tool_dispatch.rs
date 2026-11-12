use super::*;

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
    ) -> HookAction {
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
            Some(reason) => HookAction::Terminate {
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
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> crate::tool::BoxFuture<'a, Result<ToolOutcome, crate::tool_effects::ToolEffectError>> {
        Box::pin(async move {
            use crate::tool_effects::ToolEffectError::{Fatal, Unavailable};
            self.composed
                .store(true, std::sync::atomic::Ordering::Release);
            let result = async {
                let scope = current_tool_runtime_context().ok_or_else(|| {
                    Unavailable("tool effects require a request execution scope".to_owned())
                })?;
                scope_request_tool_execution_with_session(
                    scope.deadline_at,
                    scope.cancellation_token,
                    scope.workspace_cwd,
                    None,
                    scope.session_id,
                    async {
                        let internal_id = self
                            .hook
                            .admit_tool_effect(&self.parent_internal_id, ordinal, name, arguments)
                            .await
                            .map_err(Unavailable)?;
                        if let Some(outcome) =
                            admit_tool_effect(self.tools.as_slice(), name, arguments)
                        {
                            let result = match self
                                .hook
                                .on_tool_admission_rejected(
                                    name,
                                    None,
                                    &internal_id,
                                    arguments,
                                    &outcome,
                                )
                                .await
                            {
                                HookAction::Continue => Ok(outcome),
                                HookAction::Terminate { reason } => Err(Fatal(reason)),
                            };
                            let cancelled = ToolOutcome::Cancelled;
                            self.hook
                                .finish_tool_effect(
                                    &internal_id,
                                    result.as_ref().err().map(|_| &cancelled),
                                )
                                .await
                                .map_err(Fatal)?;
                            return result;
                        }
                        let result = match self
                            .hook
                            .on_tool_call(name, None, &internal_id, arguments)
                            .await
                        {
                            ToolCallHookAction::Terminate { reason } => Err(Fatal(reason)),
                            ToolCallHookAction::Skip { reason } => {
                                Ok(ToolOutcome::Completed(reason))
                            }
                            ToolCallHookAction::Continue => {
                                // Once the before hook commits, await the effect to preserve ordering.
                                let writer =
                                    self.hook.foreground_live_output_writer(&internal_id).await;
                                let session = self.hook.session_id().await;
                                let outcome = dispatch_tool(
                                    self.tools.as_slice(),
                                    name,
                                    arguments.to_owned(),
                                    Some(writer),
                                    session,
                                )
                                .await;
                                match self
                                    .hook
                                    .on_tool_result(name, None, &internal_id, arguments, &outcome)
                                    .await
                                {
                                    HookAction::Continue => Ok(outcome),
                                    HookAction::Terminate { reason } => Err(Fatal(reason)),
                                }
                            }
                        };
                        let cancelled = ToolOutcome::Cancelled;
                        self.hook
                            .finish_tool_effect(
                                &internal_id,
                                result.as_ref().err().map(|_| &cancelled),
                            )
                            .await
                            .map_err(Fatal)?;
                        result
                    },
                )
                .await
            }
            .await;
            if let Err(Fatal(reason)) = &result {
                self.fatal
                    .lock()
                    .await
                    .get_or_insert_with(|| reason.clone());
            }
            result
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
) -> Option<ToolOutcome> {
    let tool = tools.iter().find(|tool| tool.name() == name)?;
    tool.admit(args)
        .err()
        .map(|error| ToolOutcome::from_dispatch(name, Err(error)))
}

fn admit_tool_effect(tools: &[Box<dyn ToolDyn>], name: &str, args: &str) -> Option<ToolOutcome> {
    if !tools.iter().any(|tool| tool.name() == name) {
        return Some(ToolOutcome::from_dispatch(
            name,
            Err(crate::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                text: format!("tool '{name}' is not in this request's granted tool surface"),
            }),
        ));
    }
    admit_tool(tools, name, args)
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

pub async fn dispatch_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::live_output::LiveToolOutputWriter>,
    session_id: Option<String>,
) -> ToolOutcome {
    dispatch_provider_tool(tools, name, args, live_output, session_id, false, None).await
}

pub(super) async fn dispatch_provider_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::live_output::LiveToolOutputWriter>,
    session_id: Option<String>,
    token_limit: bool,
    cleanup: Option<crate::session_hook::NativeCleanupCheck>,
) -> ToolOutcome {
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        return ToolOutcome::from_dispatch(
            name,
            Err(crate::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::ArgumentInvalid,
                text: format!("error: unknown tool '{name}'"),
            }),
        );
    };

    let outcome = |result: Result<String, crate::tool::ToolError>| {
        ToolOutcome::from_dispatch(
            name,
            result.map_err(|error| crate::tool::with_provider_token_limit(error, token_limit)),
        )
    };

    let Some(scope) = current_tool_runtime_context() else {
        return outcome(
            crate::tool_call_lifecycle::runtime::dispatch_with_receipt(
                tool.as_ref(),
                args,
                live_output,
            )
            .await,
        );
    };

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

    let receipt_writer = live_output.clone();
    let mut call = Box::pin(scope_request_tool_execution_with_session(
        scope.deadline_at,
        scope.cancellation_token.clone(),
        scope.workspace_cwd.clone(),
        live_output,
        session_id.or(scope.session_id.clone()),
        tool.call_with_receipt(args),
    ));
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
    Box::new(move |hook_token, cleanup| {
        Box::pin(async move {
            let token = hook_token.child_token();
            let execute = crate::tool_effects::scope(
                input
                    .effects
                    .map(|value| value as Arc<dyn crate::tool_effects::ToolEffectDispatcher>),
                dispatch_provider_tool(
                    input.tools.as_slice(),
                    &input.name,
                    input.args,
                    input.live_output,
                    input.session_id,
                    input.token_limit,
                    cleanup,
                ),
            );
            let scoped = async {
                match input.scope {
                Some(scope) => crate::tool_call_lifecycle::runtime::scope_tool_request_identity(
                    scope.requester_did, scope.node_did, scope.agent_did, scope.request_id,
                    crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                        scope.deadline_at, token.clone(), scope.workspace_cwd, scope.live_output,
                        scope.session_id, scope.correlation, scope.source_fields, scope.background, execute,
                    ),
                ).await,
                None => tokio::select! {
                    biased;
                    _ = token.cancelled() => ToolOutcome::Cancelled,
                    outcome = execute => outcome,
                },
            }
            };
            match input.batch_token {
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
) -> HookAction
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
    use crate::tool::{BoxFuture, ToolDefinition, ToolError};
    use crate::tool_call_lifecycle::runtime::scope_request_tool_execution;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ArgumentFixture {
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        mode: Option<ArgumentMode>,
    }

    #[derive(Debug, serde::Deserialize)]
    enum ArgumentMode {
        Normal,
    }

    struct ParseOnlyTool;

    impl crate::tool::Tool for ParseOnlyTool {
        const NAME: &'static str = "parse";
        type Error = ToolError;
        type Args = ArgumentFixture;
        type Output = String;

        async fn definition(&self, _: String) -> ToolDefinition {
            ToolDefinition {
                name: "parse".into(),
                description: String::new(),
                parameters: serde_json::json!({"type":"object"}),
            }
        }

        async fn call(&self, _: Self::Args) -> Result<String, Self::Error> {
            panic!("invalid arguments must never execute")
        }
    }

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
                    let call = dispatch_provider_tool(
                        &tools,
                        "parse",
                        case.arguments.clone(),
                        None,
                        None,
                        activity.token_limit_reached(),
                        None,
                    );
                    let outcome = if scoped {
                        scope_request_tool_execution(
                            None,
                            tokio_util::sync::CancellationToken::new(),
                            call,
                        )
                        .await
                    } else {
                        call.await
                    };
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

    struct EffectHook {
        invoked: AtomicBool,
        rejected: AtomicBool,
        block_before: bool,
        cleanup_fails: bool,
        cleaned: AtomicBool,
        parent_settled: AtomicBool,
        terminate_before: bool,
    }

    #[async_trait::async_trait]
    impl SessionHook for EffectHook {
        type Error = std::convert::Infallible;

        async fn admit_tool_effect(
            &self,
            _: &str,
            _: u32,
            _: &str,
            _: &str,
        ) -> Result<String, String> {
            Ok("derived-call".into())
        }
        async fn finish_tool_effect(
            &self,
            _: &str,
            interruption: Option<&ToolOutcome>,
        ) -> Result<(), String> {
            if interruption.is_some() {
                self.cleaned.store(true, Ordering::SeqCst);
                if self.cleanup_fails {
                    return Err("durable cleanup unavailable".into());
                }
            }
            Ok(())
        }
        async fn on_completion_call_with_context(
            &self,
            _: &gents_protocol::message::Message,
            _: &[gents_protocol::message::Message],
            _: Option<&gents_protocol::message::Message>,
        ) -> HookAction {
            HookAction::Continue
        }
        async fn on_tool_call(
            &self,
            _: &str,
            _: Option<String>,
            _: &str,
            _: &str,
        ) -> ToolCallHookAction {
            self.invoked.store(true, Ordering::SeqCst);
            if self.terminate_before {
                return ToolCallHookAction::Terminate {
                    reason: "child hook terminated".into(),
                };
            }
            if self.block_before {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            ToolCallHookAction::Skip {
                reason: "specialized side effect".into(),
            }
        }
        async fn on_tool_admission_rejected(
            &self,
            _: &str,
            _: Option<String>,
            _: &str,
            _: &str,
            _: &ToolOutcome,
        ) -> HookAction {
            self.rejected.store(true, Ordering::SeqCst);
            HookAction::Continue
        }
        async fn on_tool_result(
            &self,
            _: &str,
            _: Option<String>,
            internal_id: &str,
            _: &str,
            _: &ToolOutcome,
        ) -> HookAction {
            if internal_id == "parent" {
                self.parent_settled.store(true, Ordering::SeqCst);
            }
            HookAction::Continue
        }
        async fn foreground_live_output_writer(
            &self,
            id: &str,
        ) -> crate::live_output::LiveToolOutputWriter {
            crate::live_output::LiveToolOutputRegistry::default()
                .writer_for(id)
                .await
        }
        async fn session_id(&self) -> Option<String> {
            None
        }
        async fn register_stream_tool_call_identity(&self, _: &str, _: &str, _: Option<&str>) {}
    }

    #[tokio::test]
    async fn unselected_specialized_tool_never_reaches_before_hook() {
        use crate::tool_effects::ToolEffectDispatcher;
        let hook = Arc::new(EffectHook {
            invoked: AtomicBool::new(false),
            rejected: AtomicBool::new(false),
            block_before: false,
            cleanup_fails: false,
            cleaned: AtomicBool::new(false),
            parent_settled: AtomicBool::new(false),
            terminate_before: false,
        });
        let dispatcher = EffectDispatcher {
            tools: Arc::new(vec![]),
            hook: hook.clone(),
            parent_internal_id: "parent".into(),
            fatal: tokio::sync::Mutex::new(None),
            composed: Default::default(),
        };
        let outcome = scope_request_tool_execution(
            None,
            tokio_util::sync::CancellationToken::new(),
            dispatcher.call(1, "agent_interrupt", "{}"),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, ToolOutcome::Failed { .. }));
        assert!(hook.rejected.load(Ordering::SeqCst));
        assert!(!hook.invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn before_hook_effect_outlasting_the_deadline_is_not_interrupted() {
        use crate::tool_effects::ToolEffectDispatcher;
        let hook = Arc::new(EffectHook {
            invoked: AtomicBool::new(false),
            rejected: AtomicBool::new(false),
            block_before: true,
            cleanup_fails: false,
            cleaned: AtomicBool::new(false),
            parent_settled: AtomicBool::new(false),
            terminate_before: false,
        });
        let dispatcher = EffectDispatcher {
            tools: Arc::new(vec![Box::new(ReadyAfterWallDeadline {
                deadline_at: Utc::now(),
                called: Arc::new(AtomicBool::new(false)),
                fails: false,
            })]),
            hook: hook.clone(),
            parent_internal_id: "parent".into(),
            fatal: tokio::sync::Mutex::new(None),
            composed: Default::default(),
        };
        let outcome = scope_request_tool_execution(
            Some(Utc::now() + chrono::Duration::milliseconds(10)),
            tokio_util::sync::CancellationToken::new(),
            dispatcher.call(1, "ready_after_wall_deadline", "{}"),
        )
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
                invoked: AtomicBool::new(false),
                rejected: AtomicBool::new(false),
                block_before: false,
                cleanup_fails,
                cleaned: AtomicBool::new(false),
                parent_settled: AtomicBool::new(false),
                terminate_before: true,
            });
            let dispatcher = Arc::new(EffectDispatcher {
                tools: Arc::new(vec![Box::new(ReadyAfterWallDeadline {
                    deadline_at: Utc::now(),
                    called: Arc::new(AtomicBool::new(false)),
                    fails: false,
                })]),
                hook: hook.clone(),
                parent_internal_id: "parent".into(),
                fatal: tokio::sync::Mutex::new(None),
                composed: Default::default(),
            });
            let outcome = scope_request_tool_execution(
                None,
                tokio_util::sync::CancellationToken::new(),
                crate::tool_effects::scope(Some(dispatcher.clone()), async {
                    let result = dispatcher.call(1, "ready_after_wall_deadline", "{}").await;
                    assert!(matches!(
                        result,
                        Err(crate::tool_effects::ToolEffectError::Fatal(_))
                    ));
                    ToolOutcome::Completed("guest swallowed the error".into())
                }),
            )
            .await;
            let action = dispatcher
                .settle_parent_result("plugin", None, "{}", &outcome)
                .await;
            assert!(hook.parent_settled.load(Ordering::SeqCst));
            let HookAction::Terminate { reason } = action else {
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

    struct ReadyAfterWallDeadline {
        deadline_at: DateTime<Utc>,
        called: Arc<AtomicBool>,
        fails: bool,
    }

    impl ToolDyn for ReadyAfterWallDeadline {
        fn name(&self) -> String {
            "ready_after_wall_deadline".into()
        }

        fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "ready_after_wall_deadline".into(),
                    description: "wall-clock deadline regression".into(),
                    parameters: serde_json::json!({"type": "object"}),
                }
            })
        }

        #[expect(
            clippy::disallowed_methods,
            reason = "the tool must become ready past the wall deadline within one poll"
        )]
        fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async move {
                self.called.store(true, Ordering::SeqCst);
                let remaining = (self.deadline_at - Utc::now()).to_std().unwrap_or_default();
                std::thread::sleep(remaining + std::time::Duration::from_millis(10));
                if self.fails {
                    Err(ToolError::ReportedFailure {
                        class: crate::tool_call_lifecycle::FailureClass::ToolReturnedError,
                        text: "tool failed".into(),
                    })
                } else {
                    Ok("late success".into())
                }
            })
        }
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
            let outcome = scope_request_tool_execution(
                Some(deadline_at),
                tokio_util::sync::CancellationToken::new(),
                dispatch_tool(&tools, "ready_after_wall_deadline", "{}".into(), None, None),
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
        use crate::tool::{Tool, ToolDefinition};

        struct StrictArgsTool {
            calls: Arc<AtomicUsize>,
        }
        #[derive(Debug, thiserror::Error)]
        #[error("strict tool error")]
        struct StrictToolError;
        #[derive(serde::Deserialize)]
        struct StrictArgs {
            #[allow(dead_code)]
            text: String,
        }
        impl Tool for StrictArgsTool {
            const NAME: &'static str = "strict";
            type Error = StrictToolError;
            type Args = StrictArgs;
            type Output = String;
            async fn definition(&self, _prompt: String) -> ToolDefinition {
                ToolDefinition {
                    name: Self::NAME.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({"type": "object"}),
                }
            }
            async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok("ran".to_string())
            }
        }

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
            let result = super::dispatch_tool(&tools, "strict", case.arguments, None, None).await;
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
