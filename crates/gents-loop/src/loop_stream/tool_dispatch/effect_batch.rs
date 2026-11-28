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
    async fn record_fatal(&self, reason: &str) {
        self.fatal
            .lock()
            .await
            .get_or_insert_with(|| reason.to_owned());
    }

    async fn finish_effect(&self, internal_id: &str, result: EffectResult) -> EffectResult {
        let interruption = result.as_ref().err().map(|_| &ToolOutcome::Cancelled);
        let result = match self
            .hook
            .finish_tool_effect(internal_id, interruption)
            .await
        {
            Ok(()) => result,
            Err(reason) => Err(ToolEffectError::Fatal(reason)),
        };
        if let Err(ToolEffectError::Fatal(reason)) = &result {
            self.record_fatal(reason).await;
        }
        result
    }

    async fn admit_effect(
        &self,
        call: &ToolEffectCall,
        child: &ToolCallEnvelope,
    ) -> Result<Admitted, ToolEffectError> {
        use ToolEffectError::{Fatal, Unavailable};
        let internal_id = self
            .hook
            .admit_tool_effect(
                &self.parent_internal_id,
                call.ordinal,
                &call.name,
                &call.arguments,
                child,
            )
            .await
            .map_err(Unavailable)?;
        let ready =
            match admit_tool_effect(self.tools.as_slice(), &call.name, &call.arguments, child) {
                Some(outcome) => Some(
                    match self
                        .hook
                        .on_tool_admission_rejected(
                            &call.name,
                            None,
                            &internal_id,
                            &call.arguments,
                            &outcome,
                        )
                        .await
                    {
                        HookAction::Continue => Ok(outcome),
                        HookAction::Terminate { reason } => Err(Fatal(reason)),
                    },
                ),
                None => match self
                    .hook
                    .on_tool_call(&call.name, None, &internal_id, &call.arguments, child)
                    .await
                {
                    ToolCallHookAction::Continue => None,
                    ToolCallHookAction::Skip { reason } => Some(Ok(ToolOutcome::Completed(reason))),
                    ToolCallHookAction::Terminate { reason } => Some(Err(Fatal(reason))),
                },
            };
        match ready {
            None => Ok(Admitted {
                internal_id,
                ready: None,
            }),
            Some(ready) => match self.finish_effect(&internal_id, ready).await {
                Err(Fatal(reason)) => Err(Fatal(reason)),
                result => Ok(Admitted {
                    internal_id,
                    ready: Some(result),
                }),
            },
        }
    }

    async fn run_effect(
        &self,
        call: &ToolEffectCall,
        admitted: Admitted,
        child: &ToolCallEnvelope,
    ) -> EffectResult {
        if let Some(result) = admitted.ready {
            return result;
        }
        let writer = self
            .hook
            .foreground_live_output_writer(&admitted.internal_id)
            .await;
        let session = self.hook.session_id().await;
        let outcome = dispatch_tool(
            self.tools.as_slice(),
            &call.name,
            call.arguments.clone(),
            Some(writer),
            session,
            child,
        )
        .await;
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
            crate::ToolResultHookAction::Continue => Ok(outcome),
            crate::ToolResultHookAction::ReplaceOutcome(replaced) => Ok(*replaced),
            crate::ToolResultHookAction::Terminate { reason } => {
                Err(ToolEffectError::Fatal(reason))
            }
        };
        self.finish_effect(&admitted.internal_id, result).await
    }

    /// Settles a read group whose reservation `failure` refused a member. No
    /// body in the group has started. Returns whether settling one failed.
    async fn void_group(
        &self,
        admitted: Vec<Admitted>,
        failure: &ToolEffectError,
        results: &mut Vec<EffectResult>,
    ) -> bool {
        let mut fatal = matches!(failure, ToolEffectError::Fatal(_));
        for item in admitted {
            let result = match item.ready {
                Some(ready) => ready,
                None => match failure {
                    ToolEffectError::Fatal(reason) => {
                        self.finish_effect(
                            &item.internal_id,
                            Err(ToolEffectError::Fatal(reason.clone())),
                        )
                        .await
                    }
                    ToolEffectError::Unavailable(_) => {
                        match self
                            .hook
                            .finish_tool_effect(&item.internal_id, Some(&ToolOutcome::Cancelled))
                            .await
                        {
                            Ok(()) => Ok(ToolOutcome::Cancelled),
                            Err(reason) => {
                                fatal = true;
                                self.record_fatal(&reason).await;
                                Err(ToolEffectError::Fatal(reason))
                            }
                        }
                    }
                },
            };
            results.push(result);
        }
        fatal
    }

    pub(super) async fn effect_batch(
        &self,
        parent: &ToolCallEnvelope,
        calls: Vec<ToolEffectCall>,
    ) -> Vec<EffectResult> {
        use ToolEffectError::{Fatal, Unavailable};
        if calls.is_empty() {
            return Vec::new();
        }
        self.composed
            .store(true, std::sync::atomic::Ordering::Release);
        let Some(scope) = parent.runtime() else {
            return calls
                .iter()
                .map(|_| {
                    Err(Unavailable(
                        "tool effects require a request execution scope".to_owned(),
                    ))
                })
                .collect();
        };
        let mut results = Vec::with_capacity(calls.len());
        while results.len() < calls.len() {
            let token = scope.cancellation_token.child_token();
            let child = child_envelope_under(parent, &scope, token.clone());
            let rest = &calls[results.len()..];
            let group = &rest[..crate::tool_effects::parallel::effect_width(
                rest,
                self.tools.as_slice(),
                &child,
            )];
            let mut admitted = Vec::with_capacity(group.len());
            let mut failure = None;
            for call in group {
                match self.admit_effect(call, &child).await {
                    Ok(item) => admitted.push(item),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            match failure {
                None => {}
                Some(Unavailable(reason)) if group.len() == 1 => {
                    results.push(Err(Unavailable(reason)));
                    continue;
                }
                Some(Fatal(reason)) => {
                    let failure = Fatal(reason.clone());
                    self.void_group(admitted, &failure, &mut results).await;
                    results.push(Err(Fatal(reason)));
                    break;
                }
                Some(Unavailable(reason)) => {
                    let group_end = results.len() + group.len();
                    let failure = Unavailable(reason.clone());
                    let fatal = self.void_group(admitted, &failure, &mut results).await;
                    results.resize_with(group_end, || Err(Unavailable(reason.clone())));
                    if fatal {
                        break;
                    }
                    continue;
                }
            }
            let outputs = join_all(group.iter().zip(admitted).map(|(call, item)| {
                let token = token.clone();
                let child = &child;
                async move {
                    let result = self.run_effect(call, item, child).await;
                    if matches!(result, Err(Fatal(_))) {
                        token.cancel();
                    }
                    result
                }
            }))
            .await;
            let fatal = outputs.iter().any(|result| matches!(result, Err(Fatal(_))));
            results.extend(outputs);
            if fatal {
                break;
            }
        }
        results
    }
}

#[cfg(test)]
mod tests;
