use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot};

use super::{tool_calls, BoundDir, PluginBudget, PluginOutcome, PluginRunner, PluginVerdict};

#[derive(Debug, thiserror::Error)]
pub(super) enum HostCallError {
    #[error("the plugin invocation stopped")]
    Stopped,
    #[error("plugin worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}

struct Call {
    bytes: Vec<u8>,
    reply: oneshot::Sender<Vec<u8>>,
}

/// Only the blocking WASM worker uses this end. The receiver and every reply
/// are owned by the async invocation, so dropping it releases a suspended guest.
#[derive(Clone)]
pub(super) struct Bridge(mpsc::Sender<Call>);

impl Bridge {
    pub(super) fn call(&self, bytes: Vec<u8>) -> Result<Vec<u8>, HostCallError> {
        let (reply, answer) = oneshot::channel();
        self.0
            .blocking_send(Call { bytes, reply })
            .map_err(|_| HostCallError::Stopped)?;
        answer.blocking_recv().map_err(|_| HostCallError::Stopped)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolRequest {
    tool_name: String,
    arguments: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SleepRequest {
    sleep_ms: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Request {
    Tool(ToolRequest),
    Sleep(SleepRequest),
}

fn wall_clock_for_invocation(
    declared: Duration,
    background: bool,
    remaining: Option<Duration>,
) -> Duration {
    match remaining {
        None => declared,
        Some(remaining) if background => remaining,
        Some(remaining) => declared.min(remaining),
    }
}

/// A background invocation already has an admitted lifetime on its tool row.
/// Host-call guests use that bound, including time spent awaiting host tools;
/// the manifest wall-clock allowance continues to bound foreground execution.
pub(super) fn budget_for_invocation(mut budget: PluginBudget) -> PluginBudget {
    if let Some(context) = gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context()
    {
        let remaining = context
            .deadline_at
            .map(|deadline| (deadline - chrono::Utc::now()).to_std().unwrap_or_default());
        budget.wall_clock =
            wall_clock_for_invocation(budget.wall_clock, context.background, remaining);
    }
    budget
}

fn sleep_admissible(milliseconds: u64, remaining: Duration) -> bool {
    u128::from(milliseconds) <= remaining.as_millis()
}

async fn serve(
    bytes: &[u8],
    tools: &mut Option<tool_calls::Session>,
    deadline: tokio::time::Instant,
) -> Result<Value, String> {
    match serde_json::from_slice::<Request>(bytes) {
        Ok(Request::Tool(request)) => match tools {
            Some(tools) => tools.call(&request.tool_name, request.arguments).await,
            None => Ok(json!({"error": "tool access is disabled for this invocation"})),
        },
        Ok(Request::Sleep(request)) => {
            if !sleep_admissible(
                request.sleep_ms,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            ) {
                return Ok(
                    json!({"error": "sleep exceeds the invocation's remaining wall-clock budget"}),
                );
            }
            tokio::time::sleep(Duration::from_millis(request.sleep_ms)).await;
            Ok(json!({"result": null}))
        }
        Err(error) => Ok(
            json!({"error": format!("expected {{tool_name, arguments}} or {{sleep_ms}}: {error}")}),
        ),
    }
}

pub(super) async fn run(
    runner: Arc<PluginRunner>,
    bound: Option<BoundDir>,
    input: Value,
    budget: PluginBudget,
    mut tools: Option<tool_calls::Session>,
) -> Result<PluginOutcome, HostCallError> {
    let input = prepare_input(input, &mut tools)?;
    let (sender, mut receiver) = mpsc::channel::<Call>(1);
    let deadline = tokio::time::Instant::now() + budget.wall_clock;
    let started = std::time::Instant::now();
    let mut worker = tokio::task::spawn_blocking(move || {
        let (input, manifold) = match bound {
            Some(bound) => runner.prepare_bound(&input, &bound)?,
            None => (input, runner.manifold.clone()),
        };
        runner.call_with_host(&input, &budget, manifold, Some(Bridge(sender)))
    });
    let failure = |verdict, diagnostics| PluginOutcome {
        verdict,
        diagnostics,
        output: Value::Null,
        fuel_used: 0,
        wall_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    loop {
        tokio::select! {
            result = &mut worker => return Ok(result??),
            _ = tokio::time::sleep_until(deadline) => return Ok(failure(PluginVerdict::Timeout, "the plugin exceeded its wall-clock budget".into())),
            Some(call) = receiver.recv() => {
                let response = tokio::time::timeout_at(deadline, serve(&call.bytes, &mut tools, deadline)).await;
                match response {
                    Ok(Ok(value)) => { let _ = call.reply.send(serde_json::to_vec(&value).map_err(anyhow::Error::from)?); }
                    Ok(Err(reason)) => return Ok(failure(PluginVerdict::Failed, reason)),
                    Err(_) => return Ok(failure(PluginVerdict::Timeout, "the plugin exceeded its wall-clock budget".into())),
                }
            }
        }
    }
}

pub(super) fn prepare_input(
    mut input: Value,
    tools: &mut Option<tool_calls::Session>,
) -> Result<Value, HostCallError> {
    let fields = input
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("a host-call plugin takes a JSON object"))?;
    if fields.get("with_tools") == Some(&Value::Bool(false)) {
        *tools = None;
    }
    fields.insert("tool_calls".into(), Value::Bool(tools.is_some()));
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::call::lifecycle::ToolOutcome;
    use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};

    struct Tools;

    #[tokio::test]
    #[ignore = "set GENTS_SCRIPT_WASM to the script pack's compiled WASM"]
    async fn script_pack_runs_javascript_through_the_wasm_host_bridge() {
        use sha2::Digest;
        let wasm = std::fs::read(std::env::var_os("GENTS_SCRIPT_WASM").expect("GENTS_SCRIPT_WASM"))
            .unwrap();
        let bytes = crate::plugin::tests::build_plugin_afb_bytes(&wasm);
        let (home, mut record) = crate::plugin::tests::executor::installed_echo();
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        crate::plugin::store::store_bytes(home.path(), &digest, &bytes).unwrap();
        record.digest = format!("sha256:{digest}");
        record.declaration.host_imports = vec![crate::pack::HostImport::HostCall];
        crate::plugin::store::write_record(home.path(), &record).unwrap();
        let executor = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
        let call = executor.call_data_bound_for(&record, json!({"script": "let sum=0; for(let i=0;i<3;i++) sum += (await tools.query({})).ordinal; await sleep(1); return {sum};"}), None, true, None);
        let result = gents_loop::tool_effects::scope(Some(Arc::new(Tools)), call)
            .await
            .unwrap();
        assert_eq!(
            result.outcome.verdict,
            PluginVerdict::Success,
            "{}",
            result.outcome.diagnostics
        );
        assert_eq!(result.outcome.output, json!({"sum":6}));
        let result = executor
            .call_data_bound_for(
                &record,
                json!({"script":"return typeof tools", "tool_calls":true}),
                None,
                false,
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.output, "undefined");
    }

    impl ToolEffectDispatcher for Tools {
        fn call<'a>(
            &'a self,
            ordinal: u32,
            name: &'a str,
            arguments: &'a str,
        ) -> gents_loop::tool::BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
            Box::pin(async move {
                assert!(gents_loop::tool_effects::current().is_none());
                assert_eq!(name, "query");
                assert_eq!(arguments, "{}");
                Ok(ToolOutcome::Completed(
                    json!({"ordinal": ordinal}).to_string(),
                ))
            })
        }
    }

    async fn execute(request: Value, count: u32, grant: bool, input: Value) -> PluginOutcome {
        let (home, mut record) = crate::plugin::tests::executor::installed_plugin(
            &crate::plugin::tests::executor::host_call_plugin_wat(&request, count),
            None,
        );
        record.declaration.host_imports = vec![crate::pack::HostImport::HostCall];
        crate::plugin::store::write_record(home.path(), &record).unwrap();
        let executor = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
        gents_loop::tool_effects::scope(
            Some(Arc::new(Tools)),
            executor.call_data_bound_for(&record, input, None, grant, None),
        )
        .await
        .unwrap()
        .outcome
    }

    #[tokio::test]
    async fn live_guest_preserves_state_across_sequential_tools() {
        let outcome = execute(
            json!({"tool_name": "query", "arguments": {}}),
            3,
            true,
            json!({}),
        )
        .await;
        assert_eq!(outcome.verdict, PluginVerdict::Success);
        assert_eq!(outcome.output, json!({"result": {"ordinal": 3}}));
    }

    #[tokio::test]
    async fn caller_data_cannot_enable_tools_and_can_disable_them() {
        for (grant, input) in [
            (false, json!({"tool_calls": true})),
            (true, json!({"with_tools": false})),
        ] {
            let outcome = execute(
                json!({"tool_name": "query", "arguments": {}}),
                1,
                grant,
                input,
            )
            .await;
            assert_eq!(
                outcome.output,
                json!({"error": "tool access is disabled for this invocation"})
            );
        }
    }

    #[tokio::test]
    async fn sleep_is_available_without_tool_authority() {
        let outcome = execute(json!({"sleep_ms": 1}), 1, false, json!({})).await;
        assert_eq!(outcome.output, json!({"result": null}));
        let outcome = execute(json!({"sleep_ms": u64::MAX}), 1, false, json!({})).await;
        assert!(outcome.output["error"]
            .as_str()
            .unwrap()
            .contains("remaining wall-clock budget"));
    }

    #[tokio::test]
    async fn background_sleep_uses_the_admitted_tool_lifetime() {
        let outcome = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
            tokio_util::sync::CancellationToken::new(), None, None, None, None,
            Default::default(), true,
            execute(json!({"sleep_ms": 6000}), 1, false, json!({})),
        ).await;
        assert_eq!(
            outcome.verdict,
            PluginVerdict::Success,
            "{}",
            outcome.diagnostics
        );
        assert_eq!(outcome.output, json!({"result": null}));
    }

    #[test]
    fn invocation_lifetimes_conform_to_lean() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases
            ["tool_calls"]["host_call_lifetime"];
        for case in cases.as_array().unwrap() {
            let actual = wall_clock_for_invocation(
                Duration::from_secs(case["declared"].as_u64().unwrap()),
                case["background"].as_bool().unwrap(),
                case["remaining"].as_u64().map(Duration::from_secs),
            );
            assert_eq!(
                actual.as_secs(),
                case["expected"].as_u64().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn sleep_bounds_conform_to_lean() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases
            ["tool_calls"]["sleep"];
        for case in cases.as_array().unwrap() {
            assert_eq!(
                sleep_admissible(
                    case["milliseconds"].as_u64().unwrap(),
                    Duration::from_millis(case["remaining"].as_u64().unwrap())
                ),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}
