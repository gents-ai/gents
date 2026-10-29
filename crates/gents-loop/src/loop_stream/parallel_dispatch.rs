use std::collections::VecDeque;

use crate::parallel_tools::{executable_wave_len, speculative_footprint, Footprint};
use crate::session_hook::SessionHook;
use crate::tool::ToolDyn;

use super::invalid_tool_progress::InvalidToolProgress;
use super::repeated_tool_failure::{RepeatDecision, RepeatedToolFailure};
use super::tool_dispatch::{admit_tool, value_to_json_string};

/// Only complete, hook-authorized read footprints can pass this preflight.
/// Repeated calls and an exhausted invalid-call budget remain sequential so
/// those owners observe outcomes before any dependent call is dispatched.
pub(super) fn candidate_wave_len<H: SessionHook>(
    calls: &VecDeque<(rig::completion::message::ToolCall, String)>,
    tools: &[Box<dyn ToolDyn>],
    hook: &H,
    invalid: &InvalidToolProgress,
    repeated: &RepeatedToolFailure,
) -> usize {
    let mut entries = Vec::new();
    for (call, _) in calls.iter().take(invalid.remaining()) {
        let name = call.function.name.as_str();
        let args = value_to_json_string(&call.function.arguments);
        if entries.is_empty() && repeated.decide(name, &args) != RepeatDecision::Dispatch {
            return 0;
        }
        let footprint = tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| {
                speculative_footprint(
                    hook.allows_parallel_read(name),
                    tool.scheduling_footprint(&args),
                )
            })
            .unwrap_or(Footprint::Barrier);
        if matches!(footprint, Footprint::Barrier) {
            break;
        }
        if admit_tool(tools, name, &args).is_some() {
            break;
        }
        entries.push(((name.to_owned(), args), footprint));
    }
    executable_wave_len(&entries, invalid.remaining())
}

use super::tool_dispatch::{execute_foreground, EffectDispatcher};
use super::turn_threading::bounded_tool_result;
use crate::tool_call_lifecycle::ToolOutcome;
use crate::{HookAction, ToolCallHookAction};
use futures::{stream::FuturesOrdered, StreamExt};
use std::sync::Arc;

struct Call {
    raw: rig::completion::message::ToolCall,
    internal_id: String,
    name: String,
    args: String,
}

enum Prepared {
    Run(
        Call,
        Option<crate::live_output::LiveToolOutputWriter>,
        Option<String>,
    ),
    Skip(Call, String),
}

type ResultRow = (
    gents_protocol::message::ToolCall,
    String,
    Vec<gents_protocol::message::ToolResultContent>,
);

pub(super) enum WaveResult {
    Finished(Vec<ResultRow>),
    Exhausted(Vec<ResultRow>),
    Terminated(String),
}

/// Admissions and terminal publication stay ordered. The admitted reads are
/// polled together, but each result reaches the lifecycle owner in call order.
#[expect(
    clippy::too_many_arguments,
    reason = "the wave uses the loop's existing owners"
)]
pub(super) async fn dispatch_wave<H: SessionHook + 'static>(
    calls: Vec<(rig::completion::message::ToolCall, String)>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    hook: Arc<H>,
    token_limit: bool,
    profile: crate::provider_input::ProviderInputProfile,
    invalid: &mut InvalidToolProgress,
    repeated: &mut RepeatedToolFailure,
    cancellation: tokio_util::sync::CancellationToken,
) -> WaveResult {
    let mut prepared = Vec::with_capacity(calls.len());
    for (raw, internal_id) in calls {
        let name = raw.function.name.clone();
        let args = value_to_json_string(&raw.function.arguments);
        let action = hook
            .on_tool_call(&name, raw.call_id.clone(), &internal_id, &args)
            .await;
        let call = Call {
            raw,
            internal_id,
            name,
            args,
        };
        match action {
            ToolCallHookAction::Continue => {
                let writer = Some(hook.foreground_live_output_writer(&call.internal_id).await);
                let session = hook.session_id().await;
                prepared.push(Prepared::Run(call, writer, session));
            }
            ToolCallHookAction::Skip { reason } => prepared.push(Prepared::Skip(call, reason)),
            ToolCallHookAction::Terminate { reason } => {
                cancellation.cancel();
                for pending in prepared {
                    if let Prepared::Run(pending, _, _) = pending {
                        let _ = hook
                            .on_tool_result(
                                &pending.name,
                                pending.raw.call_id,
                                &pending.internal_id,
                                &pending.args,
                                &ToolOutcome::Cancelled,
                            )
                            .await;
                    }
                }
                return WaveResult::Terminated(reason);
            }
        }
    }
    type Completed<H> = (
        Call,
        Option<String>,
        Option<(
            crate::session_hook::ForegroundResult,
            Option<Arc<EffectDispatcher<H>>>,
        )>,
    );
    let running: FuturesOrdered<crate::tool::BoxFuture<'static, Completed<H>>> = prepared
        .into_iter()
        .map(|entry| {
            let tools = tools.clone();
            let hook = hook.clone();
            let cancellation = cancellation.clone();
            Box::pin(async move {
                match entry {
                    Prepared::Skip(call, reason) => (call, Some(reason), None),
                    Prepared::Run(call, writer, session) => {
                        let output = execute_foreground(
                            tools,
                            Some(hook),
                            call.name.clone(),
                            call.internal_id.clone(),
                            call.args.clone(),
                            writer,
                            session,
                            token_limit,
                            Some(cancellation),
                        )
                        .await;
                        (call, None, Some(output))
                    }
                }
            }) as crate::tool::BoxFuture<'static, Completed<H>>
        })
        .collect();
    let mut running = running;
    let mut results = Vec::new();
    let mut terminated = None;
    while let Some((call, skipped, execution)) = running.next().await {
        let content = if let Some(reason) = skipped {
            if terminated.is_none() {
                repeated.reset();
            }
            gents_protocol::message::ToolResultContent::from_tool_output(reason)
        } else {
            let (execution, effects) = execution.expect("a running call has an execution result");
            let (outcome, already_persisted) = match execution {
                crate::session_hook::ForegroundResult::Settled(outcome) => (outcome, false),
                crate::session_hook::ForegroundResult::Backgrounded(receipt) => {
                    (ToolOutcome::Completed(receipt), true)
                }
            };
            if !already_persisted {
                let action = match effects {
                    Some(effects) => {
                        effects
                            .settle_parent_result(
                                &call.name,
                                call.raw.call_id.clone(),
                                &call.args,
                                &outcome,
                            )
                            .await
                    }
                    None => {
                        hook.on_tool_result(
                            &call.name,
                            call.raw.call_id.clone(),
                            &call.internal_id,
                            &call.args,
                            &outcome,
                        )
                        .await
                    }
                };
                if let HookAction::Terminate { reason } = action {
                    if terminated.is_none() {
                        terminated = Some(reason);
                        cancellation.cancel();
                    }
                }
            }
            if terminated.is_none() {
                invalid.record(&outcome);
                let command = tools
                    .iter()
                    .find(|tool| tool.name() == call.name)
                    .is_some_and(|tool| tool.emits_command_envelope());
                repeated.record_dispatched(&call.name, &call.args, &outcome, command);
            }
            bounded_tool_result(profile, &call.name, outcome.model_facing_text())
        };
        if terminated.is_none() {
            results.push((
                crate::rig_compat::from_rig_tool_call(&call.raw),
                call.internal_id,
                content,
            ));
        }
    }
    if let Some(reason) = terminated {
        WaveResult::Terminated(reason)
    } else if invalid.exhausted() {
        WaveResult::Exhausted(results)
    } else {
        WaveResult::Finished(results)
    }
}
