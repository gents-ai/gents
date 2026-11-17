use super::*;
use crate::tool_effects::{EffectResult, ToolEffectCall, ToolEffectError};
use futures::future::join_all;

struct Admitted {
    internal_id: String,
    ready: Option<EffectResult>,
}

impl<H> EffectDispatcher<H>
where
    H: SessionHook + 'static,
{
    async fn finish_effect(&self, id: &str, result: EffectResult) -> EffectResult {
        let result = match self
            .hook
            .finish_tool_effect(id, result.as_ref().err().map(|_| &ToolOutcome::Cancelled))
            .await
        {
            Ok(()) => result,
            Err(reason) => Err(ToolEffectError::Fatal(reason)),
        };
        if let Err(ToolEffectError::Fatal(reason)) = &result {
            self.fatal
                .lock()
                .await
                .get_or_insert_with(|| reason.clone());
        }
        result
    }

    async fn admit_effect(&self, call: &ToolEffectCall) -> Result<Admitted, ToolEffectError> {
        use ToolEffectError::{Fatal, Unavailable};
        let started = std::time::Instant::now();
        let id = self
            .hook
            .admit_tool_effect(
                &self.parent_internal_id,
                call.ordinal,
                &call.name,
                &call.arguments,
            )
            .await
            .map_err(Unavailable)?;
        tracing::debug!(
            ordinal = call.ordinal,
            phase = "reserve",
            elapsed_us = started.elapsed().as_micros() as u64,
            "script effect phase"
        );
        let started = std::time::Instant::now();
        let ready = if let Some(outcome) =
            admit_tool_effect(self.tools.as_slice(), &call.name, &call.arguments)
        {
            Some(
                match self
                    .hook
                    .on_tool_admission_rejected(&call.name, None, &id, &call.arguments, &outcome)
                    .await
                {
                    HookAction::Continue => Ok(outcome),
                    HookAction::Terminate { reason } => Err(Fatal(reason)),
                },
            )
        } else {
            match self
                .hook
                .on_tool_call(&call.name, None, &id, &call.arguments)
                .await
            {
                ToolCallHookAction::Continue => None,
                ToolCallHookAction::Skip { reason } => Some(Ok(ToolOutcome::Completed(reason))),
                ToolCallHookAction::Terminate { reason } => Some(Err(Fatal(reason))),
            }
        };
        tracing::debug!(
            ordinal = call.ordinal,
            phase = "admit_and_before_hook",
            elapsed_us = started.elapsed().as_micros() as u64,
            "script effect phase"
        );
        if let Some(ready) = ready {
            match self.finish_effect(&id, ready).await {
                Err(Fatal(reason)) => Err(Fatal(reason)),
                result => Ok(Admitted {
                    internal_id: id,
                    ready: Some(result),
                }),
            }
        } else {
            Ok(Admitted {
                internal_id: id,
                ready: None,
            })
        }
    }

    async fn run_effect(&self, call: &ToolEffectCall, admitted: Admitted) -> EffectResult {
        if let Some(result) = admitted.ready {
            return result;
        }
        let started = std::time::Instant::now();
        let writer = self
            .hook
            .foreground_live_output_writer(&admitted.internal_id)
            .await;
        let session = self.hook.session_id().await;
        tracing::debug!(
            ordinal = call.ordinal,
            phase = "execution_context",
            elapsed_us = started.elapsed().as_micros() as u64,
            "script effect phase"
        );
        let started = std::time::Instant::now();
        let outcome = dispatch_tool(
            self.tools.as_slice(),
            &call.name,
            call.arguments.clone(),
            Some(writer),
            session,
        )
        .await;
        tracing::debug!(
            ordinal = call.ordinal,
            phase = "tool_body",
            elapsed_us = started.elapsed().as_micros() as u64,
            "script effect phase"
        );
        let started = std::time::Instant::now();
        let result = match self
            .hook
            .on_tool_result(
                &call.name,
                None,
                &admitted.internal_id,
                &call.arguments,
                &outcome,
            )
            .await
        {
            HookAction::Continue => Ok(outcome),
            HookAction::Terminate { reason } => Err(ToolEffectError::Fatal(reason)),
        };
        let result = self.finish_effect(&admitted.internal_id, result).await;
        tracing::debug!(
            ordinal = call.ordinal,
            phase = "result_and_finish",
            elapsed_us = started.elapsed().as_micros() as u64,
            "script effect phase"
        );
        result
    }

    pub(super) async fn effect_batch(&self, calls: Vec<ToolEffectCall>) -> Vec<EffectResult> {
        use ToolEffectError::{Fatal, Unavailable};
        if calls.is_empty() {
            return Vec::new();
        }
        self.composed
            .store(true, std::sync::atomic::Ordering::Release);
        let Some(scope) = current_tool_runtime_context() else {
            return calls
                .into_iter()
                .map(|_| {
                    Err(Unavailable(
                        "tool effects require a request execution scope".into(),
                    ))
                })
                .collect();
        };
        scope_request_tool_execution_with_session(
            scope.deadline_at,
            scope.cancellation_token.clone(),
            scope.workspace_cwd.clone(),
            None,
            scope.session_id.clone(),
            async {
                let mut results = Vec::with_capacity(calls.len());
                while results.len() < calls.len() {
                    let rest = &calls[results.len()..];
                    let width =
                        crate::tool_effects::parallel::effect_width(rest, self.tools.as_slice());
                    let group = &rest[..width];
                    let mut admitted = Vec::new();
                    let mut failure = None;
                    for call in group {
                        match self.admit_effect(call).await {
                            Ok(item) => admitted.push(item),
                            Err(error) => {
                                failure = Some(error);
                                break;
                            }
                        }
                    }
                    if let Some(Unavailable(reason)) = &failure {
                        if width == 1 {
                            results.push(Err(Unavailable(reason.clone())));
                            continue;
                        }
                        let group_end = results.len() + width;
                        // A reservation failed: no body in this read group has started.
                        // Preserve the same nonfatal reason at every failure position.
                        for item in admitted {
                            if let Some(ready) = item.ready {
                                results.push(ready);
                            } else {
                                let cleanup = self
                                    .hook
                                    .finish_tool_effect(
                                        &item.internal_id,
                                        Some(&ToolOutcome::Cancelled),
                                    )
                                    .await;
                                match cleanup {
                                    Ok(()) => results.push(Ok(ToolOutcome::Cancelled)),
                                    Err(error) => {
                                        self.fatal
                                            .lock()
                                            .await
                                            .get_or_insert_with(|| error.clone());
                                        results.push(Err(Fatal(error)));
                                    }
                                }
                            }
                        }
                        results.resize_with(group_end, || Err(Unavailable(reason.clone())));
                        if results.iter().any(|result| matches!(result, Err(Fatal(_)))) {
                            break;
                        }
                        continue;
                    }
                    if let Some(error) = failure {
                        let Fatal(reason) = &error else {
                            unreachable!("unavailable handled above");
                        };
                        for item in admitted {
                            if let Some(ready) = item.ready {
                                results.push(ready);
                            } else {
                                let cleanup = self
                                    .finish_effect(&item.internal_id, Err(Fatal(reason.clone())))
                                    .await;
                                results.push(cleanup);
                            }
                        }
                        results.push(Err(error));
                        break;
                    }
                    let token = scope.cancellation_token.child_token();
                    let outputs = join_all(group.iter().zip(admitted).map(|(call, item)| {
                        let token = token.clone();
                        let scope = current_tool_runtime_context().expect("effect scope");
                        async move {
                            let result = scope_request_tool_execution_with_session(
                                scope.deadline_at,
                                token.clone(),
                                scope.workspace_cwd,
                                None,
                                scope.session_id,
                                self.run_effect(call, item),
                            )
                            .await;
                            if matches!(result, Err(Fatal(_))) {
                                token.cancel();
                            }
                            result
                        }
                    }))
                    .await;
                    let failed = outputs.iter().any(|result| matches!(result, Err(Fatal(_))));
                    results.extend(outputs);
                    if failed {
                        break;
                    }
                }
                results
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests;
