/// Reserve invalid-call budget before dispatch and stop at any duplicate,
/// because repeated-failure decisions depend on preceding results.
pub(super) fn read_prefix<'a>(
    calls: impl IntoIterator<Item = (&'a str, &'a str, bool)>,
    limit: usize,
) -> usize {
    let mut seen = Vec::new();
    for (name, args, read) in calls.into_iter().take(limit) {
        if !read || seen.contains(&(name, args)) {
            break;
        }
        seen.push((name, args));
    }
    seen.len()
}

fn unique<T>(ids: &[T]) -> bool
where
    T: Eq,
{
    ids.iter()
        .enumerate()
        .all(|(index, id)| !ids[..index].contains(id))
}

fn fully_settled<T>(accepted: &[T], settled: &[T]) -> bool
where
    T: Eq,
{
    unique(accepted)
        && unique(settled)
        && accepted.len() == settled.len()
        && accepted.iter().all(|id| settled.contains(id))
}

fn ordered_indices<T>(accepted: &[T], settled: &[T]) -> Option<Vec<usize>>
where
    T: Eq,
{
    fully_settled(accepted, settled).then(|| {
        accepted
            .iter()
            .map(|id| {
                settled
                    .iter()
                    .position(|row| row == id)
                    .expect("complete settlement contains each call")
            })
            .collect()
    })
}

#[cfg(test)]
use super::RigToolFunction;
use super::{
    tool_dispatch, HookAction, RigToolCall as ToolCall, SessionHook, ToolCallHookAction, ToolDyn,
};
use crate::session_hook::ForegroundResult;
use crate::tool_call_lifecycle::runtime::{CurrentToolRuntimeContext, ToolOutcome};
use futures::{stream::FuturesUnordered, StreamExt};
use std::sync::Arc;

pub(super) struct BatchResult {
    pub call: ToolCall,
    pub internal_id: String,
    pub args: String,
    pub outcome: Option<ToolOutcome>,
    pub skipped: Option<String>,
    termination: Option<String>,
}

struct Admitted {
    call: ToolCall,
    internal_id: String,
    args: String,
    skipped: Option<String>,
}

pub(super) async fn run_batch<H>(
    calls: Vec<(ToolCall, String)>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    hook: Option<Arc<H>>,
    scope: Option<CurrentToolRuntimeContext>,
    token_limit: bool,
) -> (Vec<BatchResult>, Option<String>)
where
    H: SessionHook + 'static,
{
    let mut admitted = Vec::new();
    for (call, internal_id) in calls {
        let args = super::value_to_json_string(&call.function.arguments);
        let action = match hook.as_ref() {
            Some(hook) => {
                hook.on_tool_call(
                    &call.function.name,
                    call.call_id.clone(),
                    &internal_id,
                    &args,
                )
                .await
            }
            None => ToolCallHookAction::Continue,
        };
        match action {
            ToolCallHookAction::Terminate { reason } => {
                if let Some(hook) = hook.as_ref() {
                    for prior in &admitted {
                        let prior: &Admitted = prior;
                        if prior.skipped.is_none() {
                            let _ = hook
                                .on_tool_result(
                                    &prior.call.function.name,
                                    prior.call.call_id.clone(),
                                    &prior.internal_id,
                                    &prior.args,
                                    &ToolOutcome::Cancelled,
                                )
                                .await;
                        }
                    }
                }
                return (Vec::new(), Some(reason));
            }
            ToolCallHookAction::Skip { reason } => admitted.push(Admitted {
                call,
                internal_id,
                args,
                skipped: Some(reason),
            }),
            ToolCallHookAction::Continue => admitted.push(Admitted {
                call,
                internal_id,
                args,
                skipped: None,
            }),
        }
    }
    let admitted_ids: Vec<_> = admitted
        .iter()
        .map(|item| item.internal_id.clone())
        .collect();
    let accepted: Vec<_> = admitted
        .iter()
        .filter(|item| item.skipped.is_none())
        .map(|item| item.internal_id.clone())
        .collect();
    let batch_token = tokio_util::sync::CancellationToken::new();
    let mut pending: FuturesUnordered<crate::tool::BoxFuture<'static, BatchResult>> =
        FuturesUnordered::new();
    for item in admitted {
        let tools = tools.clone();
        let hook = hook.clone();
        let token = batch_token.clone();
        let scope = scope.clone();
        pending.push(Box::pin(async move {
            let Admitted {
                call,
                internal_id,
                args,
                skipped,
            } = item;
            if skipped.is_some() {
                return BatchResult {
                    call,
                    internal_id,
                    args,
                    outcome: None,
                    skipped,
                    termination: None,
                };
            }
            let live_output = match hook.as_ref() {
                Some(hook) => Some(hook.foreground_live_output_writer(&internal_id).await),
                None => None,
            };
            let session_id = match hook.as_ref() {
                Some(hook) => hook.session_id().await,
                None => None,
            };
            let effects = hook.as_ref().map(|hook| {
                Arc::new(tool_dispatch::EffectDispatcher {
                    tools: tools.clone(),
                    hook: hook.clone(),
                    parent_internal_id: internal_id.clone(),
                    fatal: tokio::sync::Mutex::new(None),
                })
            });
            let call_fn = tool_dispatch::foreground_call(tool_dispatch::ForegroundExecution {
                tools,
                name: call.function.name.clone(),
                args: args.clone(),
                live_output,
                session_id,
                effects: effects.clone(),
                scope,
                token_limit,
                batch_token: Some(token.clone()),
            });
            let execution = match hook.as_ref() {
                Some(hook) => {
                    hook.run_foreground_tool(&call.function.name, &internal_id, call_fn)
                        .await
                }
                None => ForegroundResult::Settled(call_fn(Default::default(), None).await),
            };
            let (outcome, already_persisted) = match execution {
                ForegroundResult::Settled(outcome) => (outcome, false),
                ForegroundResult::Backgrounded(receipt) => (ToolOutcome::Completed(receipt), true),
            };
            let termination = if let Some(hook) = hook.as_ref().filter(|_| !already_persisted) {
                match tool_dispatch::settle_foreground(
                    hook.as_ref(),
                    effects.as_ref(),
                    &call.function.name,
                    call.call_id.clone(),
                    &internal_id,
                    &args,
                    &outcome,
                )
                .await
                {
                    HookAction::Continue => None,
                    HookAction::Terminate { reason } => {
                        token.cancel();
                        Some(reason)
                    }
                }
            } else {
                None
            };
            BatchResult {
                call,
                internal_id,
                args,
                outcome: Some(outcome),
                skipped: None,
                termination,
            }
        }));
    }
    let mut results = Vec::new();
    let mut termination = None;
    while let Some(result) = pending.next().await {
        if let Some(reason) = result.termination.as_ref() {
            termination.get_or_insert_with(|| reason.clone());
        }
        results.push(result);
    }
    let settled: Vec<_> = results
        .iter()
        .filter(|result| result.outcome.is_some())
        .map(|result| result.internal_id.clone())
        .collect();
    if !fully_settled(&accepted, &settled) {
        termination.get_or_insert_with(|| {
            "tool batch did not settle every accepted call exactly once".to_string()
        });
    }
    let observed_ids: Vec<_> = results
        .iter()
        .map(|result| result.internal_id.clone())
        .collect();
    if let Some(indices) = ordered_indices(&admitted_ids, &observed_ids) {
        let mut slots: Vec<_> = results.into_iter().map(Some).collect();
        results = indices
            .into_iter()
            .map(|index| slots[index].take().expect("unique settlement index"))
            .collect();
    } else {
        termination.get_or_insert_with(|| "tool batch cannot order its settlements".to_string());
    }
    (results, termination)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use super::{ordered_indices, read_prefix};

    #[test]
    fn read_prefix_conforms_to_executable_lean_owner() {
        let cases = [
            (4, vec![("read", "a", true), ("read", "b", true)]),
            (
                4,
                vec![
                    ("read", "a", true),
                    ("write", "a", false),
                    ("read", "b", true),
                ],
            ),
            (4, vec![("unknown", "a", false), ("read", "b", true)]),
            (4, vec![("read", "a", true), ("read", "a", true)]),
            (1, vec![("read", "a", true), ("read", "b", true)]),
            (0, vec![("read", "a", true)]),
            (4, vec![("read", "a", true), ("read", "b", true)]),
            (4, vec![("read", "a", true), ("read", "b", true)]),
        ];
        let mut inputs = Vec::new();
        let mut actual = Vec::new();
        let settlements = [
            (vec![0, 1], vec![(1, 20), (0, 10)], "none"),
            (vec![0, 1], vec![(0, 10)], "cancelled"),
            (vec![0, 1], vec![(0, 10), (1, 20)], "terminated"),
            (vec![0, 1], vec![(0, 10), (0, 11)], "terminated"),
            (vec![0, 0], vec![(0, 10)], "none"),
            (vec![], vec![], "cancelled"),
            (vec![0, 1], vec![(1, 10)], "terminated"),
            (vec![0, 1], vec![(1, 10), (0, 0)], "terminated"),
        ];
        for ((limit, calls), (accepted, settled, stop)) in cases.into_iter().zip(settlements) {
            let mut identities = Vec::new();
            let modeled: Vec<_> = calls
                .iter()
                .map(|(name, args, read)| {
                    let pair = (*name, *args);
                    let key = match identities.iter().position(|existing| *existing == pair) {
                        Some(key) => key,
                        None => {
                            identities.push(pair);
                            identities.len() - 1
                        }
                    };
                    serde_json::json!({"key": key, "read": read})
                })
                .collect();
            let ids: Vec<_> = settled.iter().map(|row| row.0).collect();
            let indices = ordered_indices(&accepted, &ids);
            let complete = indices.is_some();
            let ordered = indices.map(|indices| {
                indices
                    .into_iter()
                    .map(|index| (settled[index].0, Some(settled[index].1)))
                    .collect::<Vec<_>>()
            });
            inputs.push(serde_json::json!({"limit": limit, "calls": modeled, "accepted": accepted, "settled": settled, "stop": stop, "actions": []}));
            actual.push(serde_json::json!({"length": read_prefix(calls, limit), "complete": complete, "ordered": ordered, "admitted": []}));
        }
        let proof_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/proofs");
        gents_lean_contract::run_lake_build(&proof_dir).expect("build Lean contract");
        let mut lean = Command::new("lake")
            .args([
                "env",
                "lean",
                "--run",
                "Proofs/Conformance/ParallelToolBatch.lean",
            ])
            .current_dir(proof_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start Lean owner");
        writeln!(lean.stdin.take().unwrap(), "{}", serde_json::json!(inputs)).unwrap();
        let output = lean.wait_with_output().expect("run Lean owner");
        assert!(output.status.success(), "Lean owner failed");
        let mut expected: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).expect("Lean results");
        for case in &mut expected {
            case.as_object_mut().unwrap().remove("targets");
        }
        assert_eq!(actual, expected);
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use crate::tool::{BoxFuture, ToolDefinition, ToolError};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Probe {
        calls: Arc<AtomicUsize>,
        pending_second: bool,
    }
    impl ToolDyn for Probe {
        fn name(&self) -> String {
            "probe".into()
        }
        fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "probe".into(),
                    description: String::new(),
                    parameters: serde_json::json!({"type":"object"}),
                }
            })
        }
        fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.pending_second && args.contains('1') {
                    std::future::pending().await
                } else {
                    Ok(args)
                }
            })
        }
    }

    struct Hook {
        actions: Vec<&'static str>,
        settled: tokio::sync::Mutex<Vec<(usize, bool)>>,
        terminate_id: Option<usize>,
        cancelled_terminates: bool,
        cancel_token: bool,
        request_tokens: tokio::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>,
    }
    #[async_trait::async_trait]
    impl SessionHook for Hook {
        type Error = std::convert::Infallible;

        async fn run_foreground_tool(
            &self,
            _: &str,
            _: &str,
            call: crate::session_hook::ForegroundCall,
        ) -> ForegroundResult {
            let token = tokio_util::sync::CancellationToken::new();
            if self.cancel_token {
                token.cancel();
            }
            self.request_tokens.lock().await.push(token.clone());
            ForegroundResult::Settled(call(token, None).await)
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
            id: &str,
            _: &str,
        ) -> ToolCallHookAction {
            match self.actions[id.parse::<usize>().unwrap()] {
                "skip" => ToolCallHookAction::Skip {
                    reason: "skipped".into(),
                },
                "terminate" => ToolCallHookAction::Terminate {
                    reason: "stopped".into(),
                },
                _ => ToolCallHookAction::Continue,
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
            HookAction::Continue
        }
        async fn on_tool_result(
            &self,
            _: &str,
            _: Option<String>,
            id: &str,
            _: &str,
            outcome: &ToolOutcome,
        ) -> HookAction {
            let id = id.parse::<usize>().unwrap();
            self.settled
                .lock()
                .await
                .push((id, matches!(outcome, ToolOutcome::Cancelled)));
            if self.cancelled_terminates && matches!(outcome, ToolOutcome::Cancelled) {
                HookAction::Terminate {
                    reason: "cancelled".into(),
                }
            } else if self.terminate_id == Some(id) {
                HookAction::Terminate {
                    reason: "stopped".into(),
                }
            } else {
                HookAction::Continue
            }
        }
        async fn foreground_live_output_writer(
            &self,
            id: &str,
        ) -> crate::live_output::LiveToolOutputWriter {
            crate::live_output::LiveToolOutputRegistry::default()
                .writer_for(id.to_string())
                .await
        }
        async fn session_id(&self) -> Option<String> {
            None
        }
        async fn register_stream_tool_call_identity(&self, _: &str, _: &str, _: Option<&str>) {}
    }

    fn calls(count: usize) -> Vec<(ToolCall, String)> {
        (0..count)
            .map(|id| {
                (
                    ToolCall {
                        id: id.to_string(),
                        call_id: Some(id.to_string()),
                        function: RigToolFunction {
                            name: "probe".into(),
                            arguments: serde_json::json!({"id": id}),
                        },
                        signature: None,
                        additional_params: None,
                    },
                    id.to_string(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn ordered_admission_and_cancellation_settle_every_accepted_call() {
        for (actions, terminate_result, expected_cancelled) in [
            (vec!["run", "skip", "run"], false, false),
            (vec!["run", "terminate", "run"], false, true),
            (vec!["run", "run"], true, true),
        ] {
            let model_actions = actions.clone();
            let expected: Vec<usize> = tokio::task::spawn_blocking(move || {
                use std::io::Write;
                use std::process::{Command, Stdio};
                let proof_dir = gents_lean_contract::proofs_dir().unwrap();
                gents_lean_contract::run_lake_build(&proof_dir).unwrap();
                let mut lean = Command::new("lake").args(["env", "lean", "--run", "Proofs/Conformance/ParallelToolBatch.lean"]).current_dir(proof_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
                writeln!(lean.stdin.take().unwrap(), "{}", serde_json::json!([{"limit": 4, "calls": [], "accepted": [], "settled": [], "stop": "none", "actions": model_actions}])).unwrap();
                let output = lean.wait_with_output().unwrap();
                assert!(output.status.success());
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                serde_json::from_value(value[0]["admitted"].clone()).unwrap()
            }).await.unwrap();
            let hook = Arc::new(Hook {
                actions: actions.clone(),
                settled: tokio::sync::Mutex::new(Vec::new()),
                terminate_id: terminate_result.then_some(0),
                cancelled_terminates: false,
                cancel_token: false,
                request_tokens: Default::default(),
            });
            let count = Arc::new(AtomicUsize::new(0));
            let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(Probe {
                calls: count.clone(),
                pending_second: terminate_result,
            })]);
            let (results, termination) = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                run_batch(calls(actions.len()), tools, Some(hook.clone()), None, false),
            )
            .await
            .expect("accepted calls settle");
            let settled = hook.settled.lock().await.clone();
            let mut settled_ids = settled.iter().map(|row| row.0).collect::<Vec<_>>();
            settled_ids.sort_unstable();
            assert_eq!(settled_ids, expected);
            if expected_cancelled {
                assert!(termination.is_some());
                assert!(settled.iter().any(|row| row.1));
            } else {
                assert!(termination.is_none());
                assert_eq!(results.len(), actions.len());
            }
            if actions.contains(&"terminate") {
                assert_eq!(count.load(Ordering::SeqCst), 0);
            }
        }
    }
    struct HeldFirst {
        entered: Arc<tokio::sync::Notify>,
    }

    impl ToolDyn for HeldFirst {
        fn name(&self) -> String {
            "probe".into()
        }
        fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "probe".into(),
                    description: String::new(),
                    parameters: serde_json::json!({"type": "object"}),
                }
            })
        }
        fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async move {
                if args.contains('0') {
                    self.entered.notify_one();
                    std::future::pending().await
                } else {
                    self.entered.notified().await;
                    Ok(args)
                }
            })
        }
    }

    #[tokio::test]
    async fn later_termination_cancels_held_earlier_call() {
        let model: serde_json::Value = tokio::task::spawn_blocking(|| {
            use std::io::Write;
            use std::process::{Command, Stdio};
            let proof_dir = gents_lean_contract::proofs_dir().unwrap();
            gents_lean_contract::run_lake_build(&proof_dir).unwrap();
            let mut lean = Command::new("lake").args(["env", "lean", "--run", "Proofs/Conformance/ParallelToolBatch.lean"])
                .current_dir(proof_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            writeln!(lean.stdin.take().unwrap(), "{}", serde_json::json!([
                {"limit": 0, "calls": [], "accepted": [0, 1], "settled": [[1, 10]], "stop": "terminated", "actions": []},
                {"limit": 0, "calls": [], "accepted": [0, 1], "settled": [[1, 10], [0, 0]], "stop": "terminated", "actions": []}
            ])).unwrap();
            let output = lean.wait_with_output().unwrap();
            assert!(output.status.success());
            serde_json::from_slice(&output.stdout).unwrap()
        }).await.unwrap();
        let hook = Arc::new(Hook {
            actions: vec!["run", "run"],
            settled: tokio::sync::Mutex::new(Vec::new()),
            terminate_id: Some(1),
            cancelled_terminates: true,
            cancel_token: false,
            request_tokens: Default::default(),
        });
        let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(HeldFirst {
            entered: Arc::new(tokio::sync::Notify::new()),
        })]);
        let (results, termination) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run_batch(calls(2), tools, Some(hook.clone()), None, false),
        )
        .await
        .expect("later termination cancels the held first call");
        assert_eq!(termination.as_deref(), Some("stopped"));
        let settled = hook.settled.lock().await.clone();
        let cancellation_targets: Vec<_> = settled
            .iter()
            .filter_map(|(id, cancelled)| cancelled.then_some(*id))
            .collect();
        assert_eq!(serde_json::json!(cancellation_targets), model[0]["targets"]);
        assert_eq!(model[0]["ordered"], serde_json::Value::Null);
        let observed_order: Vec<_> = results
            .iter()
            .map(|result| {
                (
                    result.internal_id.parse::<usize>().unwrap(),
                    Some(
                        if matches!(result.outcome.as_ref(), Some(ToolOutcome::Cancelled)) {
                            0
                        } else {
                            10
                        },
                    ),
                )
            })
            .collect();
        assert_eq!(serde_json::json!(observed_order), model[1]["ordered"]);
        assert_eq!(settled, vec![(1, false), (0, true)]);
        let request_tokens = hook.request_tokens.lock().await;
        assert_eq!(request_tokens.len(), 2);
        assert!(
            request_tokens.iter().all(|token| !token.is_cancelled()),
            "batch termination cancels its calls, not the request"
        );
    }

    #[tokio::test]
    async fn batch_dispatch_respects_hook_cancellation_token() {
        let hook = Arc::new(Hook {
            actions: vec!["run", "run"],
            settled: tokio::sync::Mutex::new(Vec::new()),
            terminate_id: None,
            cancelled_terminates: false,
            cancel_token: true,
            request_tokens: Default::default(),
        });
        let count = Arc::new(AtomicUsize::new(0));
        let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(Probe {
            calls: count.clone(),
            pending_second: false,
        })]);
        let (results, termination) =
            run_batch(calls(2), tools, Some(hook.clone()), None, false).await;
        assert!(termination.is_none());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(results.len(), 2);
        let mut settled = hook.settled.lock().await.clone();
        settled.sort_unstable();
        assert_eq!(settled, vec![(0, true), (1, true)]);
    }
}
