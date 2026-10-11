use super::*;

pub(super) struct EffectDispatcher<H> {
    pub(super) tools: Arc<Vec<Box<dyn ToolDyn>>>,
    pub(super) hook: Arc<H>,
    pub(super) parent_internal_id: String,
    // Guest error conversion cannot clear a hook termination before parent settlement.
    pub(super) fatal: tokio::sync::Mutex<Option<String>>,
}

impl<H: SessionHook + 'static> EffectDispatcher<H> {
    pub(super) async fn settle_parent_result(
        &self,
        name: &str,
        call_id: Option<String>,
        arguments: &str,
        outcome: &ToolOutcome,
    ) -> HookAction {
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

impl<H: SessionHook + 'static> crate::tool_effects::ToolEffectDispatcher for EffectDispatcher<H> {
    fn call<'a>(
        &'a self,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
        budget: std::time::Duration,
    ) -> crate::tool::BoxFuture<'a, Result<ToolOutcome, crate::tool_effects::ToolEffectError>> {
        Box::pin(async move {
            use crate::tool_effects::ToolEffectError::{Fatal, Unavailable};
            let result = async {
                let scope = current_tool_runtime_context().ok_or_else(|| {
                    Unavailable("tool effects require a request execution scope".to_owned())
                })?;
                let limit = chrono::Utc::now()
                    + chrono::Duration::from_std(budget)
                        .map_err(|_| Unavailable("tool effect deadline is out of range".to_owned()))?;
                let deadline = Some(
                    scope
                        .deadline_at
                        .map_or(limit, |deadline| deadline.min(limit)),
                );
                scope_request_tool_execution_with_session(
                    deadline,
                    scope.cancellation_token,
                    scope.workspace_cwd,
                    None,
                    scope.session_id,
                    async {
                        let internal_id = self.hook
                            .admit_tool_effect(&self.parent_internal_id, ordinal, name, arguments)
                            .await
                            .map_err(Unavailable)?;
                        if let Some(outcome) = admit_tool_effect(self.tools.as_slice(), name, arguments) {
                            let result = match self.hook
                                .on_tool_admission_rejected(name, None, &internal_id, arguments, &outcome)
                                .await
                            {
                                HookAction::Continue => Ok(outcome),
                                HookAction::Terminate { reason } => Err(Fatal(reason)),
                            };
                            let cancelled = ToolOutcome::Cancelled;
                            self.hook
                                .finish_tool_effect(&internal_id, result.as_ref().err().map(|_| &cancelled))
                                .await
                                .map_err(Fatal)?;
                            return result;
                        }
                        let before = self.hook.on_tool_call(name, None, &internal_id, arguments);
                        let before = tokio::select! {
                            biased;
                            _ = current_tool_runtime_context().expect("scoped effect").cancellation_token.cancelled_owned() => Err(ToolOutcome::Cancelled),
                            _ = tokio::time::sleep(deadline_remaining(deadline).unwrap_or_default()) => Err(ToolOutcome::TimedOut { deadline_at: deadline }),
                            action = before => Ok(action),
                        };
                        let before = if before.is_ok()
                            && deadline_remaining(deadline).is_some_and(|remaining| remaining.is_zero())
                        {
                            Err(ToolOutcome::TimedOut { deadline_at: deadline })
                        } else {
                            before
                        };
                        let before = match before {
                            Ok(action) => action,
                            Err(outcome) => {
                                self.hook
                                    .finish_tool_effect(&internal_id, Some(&outcome))
                                    .await
                                    .map_err(Fatal)?;
                                return Ok(outcome);
                            }
                        };
                        let result = match before {
                            ToolCallHookAction::Terminate { reason } => Err(Fatal(reason)),
                            ToolCallHookAction::Skip { reason } => Ok(ToolOutcome::Completed(reason)),
                            ToolCallHookAction::Continue => {
                                let writer = self.hook
                                    .foreground_live_output_writer(&internal_id).await;
                                let session = self.hook.session_id().await;
                                let outcome = dispatch_tool(
                                    self.tools.as_slice(), name, arguments.to_owned(), Some(writer), session,
                                ).await;
                                match self.hook
                                    .on_tool_result(name, None, &internal_id, arguments, &outcome).await
                                {
                                    HookAction::Continue => Ok(outcome),
                                    HookAction::Terminate { reason } => Err(Fatal(reason)),
                                }
                            }
                        };
                        let cancelled = ToolOutcome::Cancelled;
                        self.hook
                            .finish_tool_effect(&internal_id, result.as_ref().err().map(|_| &cancelled))
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
}

// pub, not pub(super): gents' own tool-execution tests exercise this
// formatting helper directly (a pub(super) item in this crate is invisible to
// a dependent crate's own test build).
pub fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

/// Tool-policy admission, evaluated before the dispatch election. `Some` is
/// the pre-dispatch failure that settles the still-Pending call; the tool is
/// never invoked. Unknown names are left to `dispatch_tool`.
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

pub async fn dispatch_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::live_output::LiveToolOutputWriter>,
    session_id: Option<String>,
) -> ToolOutcome {
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        // An unresolved name is a typed dispatch failure, not completed output.
        return ToolOutcome::from_dispatch(
            name,
            Err(crate::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::ArgumentInvalid,
                text: format!("error: unknown tool '{name}'"),
            }),
        );
    };

    let Some(scope) = current_tool_runtime_context() else {
        return ToolOutcome::from_dispatch(
            name,
            crate::tool_call_lifecycle::runtime::dispatch_with_receipt(
                tool.as_ref(),
                args,
                live_output,
            )
            .await,
        );
    };

    if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
        return ToolOutcome::TimedOut {
            deadline_at: scope.deadline_at,
        };
    }

    let deadline_at = scope.deadline_at;
    let mut deadline = Box::pin(async move {
        match deadline_remaining(deadline_at) {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    });

    let receipt_writer = live_output.clone();
    let call = scope_request_tool_execution_with_session(
        scope.deadline_at,
        scope.cancellation_token.clone(),
        scope.workspace_cwd.clone(),
        live_output,
        session_id.or(scope.session_id.clone()),
        tool.call_with_receipt(args),
    );
    tokio::select! {
        biased;
        _ = scope.cancellation_token.cancelled() => ToolOutcome::Cancelled,
        _ = &mut deadline => ToolOutcome::TimedOut { deadline_at: scope.deadline_at },
        dispatched = call => {
            let result = crate::tool_call_lifecycle::runtime::record_dispatch_receipt(dispatched, receipt_writer).await;
            if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
                ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
            } else {
                ToolOutcome::from_dispatch(name, result)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{BoxFuture, ToolDefinition, ToolError};
    use crate::tool_call_lifecycle::runtime::scope_request_tool_execution;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

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
                std::future::pending::<()>().await;
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
        };
        let outcome = scope_request_tool_execution(
            None,
            tokio_util::sync::CancellationToken::new(),
            dispatcher.call(
                1,
                "agent_interrupt",
                "{}",
                std::time::Duration::from_secs(1),
            ),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, ToolOutcome::Failed { .. }));
        assert!(hook.rejected.load(Ordering::SeqCst));
        assert!(!hook.invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn deadline_during_before_hook_requires_durable_cleanup() {
        use crate::tool_effects::{ToolEffectDispatcher, ToolEffectError};
        for cleanup_fails in [false, true] {
            let hook = Arc::new(EffectHook {
                invoked: AtomicBool::new(false),
                rejected: AtomicBool::new(false),
                block_before: true,
                cleanup_fails,
                cleaned: AtomicBool::new(false),
                parent_settled: AtomicBool::new(false),
                terminate_before: false,
            });
            let tool = ReadyAfterWallDeadline {
                deadline_at: Utc::now(),
                called: Arc::new(AtomicBool::new(false)),
            };
            let dispatcher = EffectDispatcher {
                tools: Arc::new(vec![Box::new(tool)]),
                hook: hook.clone(),
                parent_internal_id: "parent".into(),
                fatal: tokio::sync::Mutex::new(None),
            };
            let result = scope_request_tool_execution(
                None,
                tokio_util::sync::CancellationToken::new(),
                dispatcher.call(
                    1,
                    "ready_after_wall_deadline",
                    "{}",
                    std::time::Duration::from_millis(10),
                ),
            )
            .await;
            assert!(hook.invoked.load(Ordering::SeqCst));
            assert!(hook.cleaned.load(Ordering::SeqCst));
            if cleanup_fails {
                assert!(matches!(result, Err(ToolEffectError::Fatal(_))));
            } else {
                assert!(matches!(result, Ok(ToolOutcome::TimedOut { .. })));
            }
        }
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
                })]),
                hook: hook.clone(),
                parent_internal_id: "parent".into(),
                fatal: tokio::sync::Mutex::new(None),
            });
            let outcome = scope_request_tool_execution(
                None,
                tokio_util::sync::CancellationToken::new(),
                crate::tool_effects::scope(Some(dispatcher.clone()), async {
                    let result = dispatcher
                        .call(
                            1,
                            "ready_after_wall_deadline",
                            "{}",
                            std::time::Duration::from_secs(1),
                        )
                        .await;
                    assert!(matches!(
                        result,
                        Err(crate::tool_effects::ToolEffectError::Fatal(_))
                    ));
                    // Even a caller converting the fatal error into successful output
                    // cannot authorize the owned loop's next provider call.
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

        fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async move {
                self.called.store(true, Ordering::SeqCst);
                let remaining = (self.deadline_at - Utc::now()).to_std().unwrap_or_default();
                // This synchronous call returns Ready on its first poll after
                // wall time advances. Tokio cannot repoll its sibling sleep
                // while the current task is inside this tool future.
                std::thread::sleep(remaining + std::time::Duration::from_millis(10));
                Ok("late success".into())
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ready_tool_after_wall_deadline_is_timed_out() {
        let deadline_at = Utc::now() + chrono::Duration::seconds(1);
        let called = Arc::new(AtomicBool::new(false));
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ReadyAfterWallDeadline {
            deadline_at,
            called: Arc::clone(&called),
        })];
        let outcome = scope_request_tool_execution(
            Some(deadline_at),
            tokio_util::sync::CancellationToken::new(),
            dispatch_tool(&tools, "ready_after_wall_deadline", "{}".into(), None, None),
        )
        .await;
        assert!(
            called.load(Ordering::SeqCst),
            "tool must reach the call-ready branch"
        );
        assert_eq!(
            outcome,
            ToolOutcome::TimedOut {
                deadline_at: Some(deadline_at)
            }
        );
    }
}
