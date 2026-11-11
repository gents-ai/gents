use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use super::tool_calls;
use gents_pack::plugin::host_bridge::{
    compute_allowance, Bridge, Call, HostCallError, Stop, StopOnDrop,
};
use gents_pack::{
    plugin::BoundDir, plugin::PluginBudget, plugin::PluginOutcome, plugin::PluginRunner,
    plugin::PluginVerdict,
};

#[cfg(test)]
pub(crate) mod live {
    //! Which guests still occupy a worker thread, by artifact digest.
    static GUESTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    pub(super) struct Guard(String);

    impl Guard {
        pub fn new(digest: String) -> Self {
            GUESTS.lock().unwrap().push(digest.clone());
            Self(digest)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let mut guests = GUESTS.lock().unwrap();
            if let Some(index) = guests.iter().position(|digest| digest == &self.0) {
                guests.swap_remove(index);
            }
        }
    }

    pub(crate) fn count(digest: &str) -> usize {
        let guests = GUESTS.lock().unwrap();
        guests
            .iter()
            .filter(|guest| guest.as_str() == digest)
            .count()
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
pub fn budget_for_invocation(mut budget: PluginBudget) -> PluginBudget {
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

pub async fn run(
    runner: Arc<PluginRunner>,
    bound: Option<BoundDir>,
    input: Value,
    budget: PluginBudget,
    declared: Duration,
    mut tools: Option<tool_calls::Session>,
) -> Result<PluginOutcome, HostCallError> {
    let input = prepare_input(input, &mut tools)?;
    let (sender, mut receiver) = mpsc::channel::<Call>(1);
    let deadline = tokio::time::Instant::now() + budget.wall_clock;
    let started = std::time::Instant::now();
    let stop = Arc::new(Stop {
        stopped: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        deadline: started + budget.wall_clock,
        slice: declared,
    });
    let _stop = StopOnDrop(stop.clone());
    let guest_budget = PluginBudget {
        wall_clock: compute_allowance(declared, budget.wall_clock, false),
        ..budget
    };
    let mut worker = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _live = live::Guard::new(runner.artifact_digest());
        let (input, manifold) = match bound {
            Some(bound) => runner.prepare_bound(&input, &bound)?,
            None => (input, runner.manifold.clone()),
        };
        let bridge = Bridge {
            calls: sender,
            stop,
        };
        runner.call_with_host(&input, &guest_budget, manifold, Some(bridge))
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

pub fn prepare_input(
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
    use gents_loop::tool_call_lifecycle::ToolOutcome;
    use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};

    struct Tools;

    #[tokio::test]
    #[ignore = "set GENTS_SCRIPT_WASM to the script pack's compiled WASM"]
    async fn script_pack_runs_javascript_through_the_wasm_host_bridge() {
        use sha2::Digest;
        let wasm = std::fs::read(std::env::var_os("GENTS_SCRIPT_WASM").expect("GENTS_SCRIPT_WASM"))
            .unwrap();
        let bytes = gents_pack::plugin::test_fixtures::build_plugin_afb_bytes(&wasm);
        let (home, mut record) = gents_pack::plugin::test_fixtures::installed_echo();
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        gents_pack::plugin::store::store_bytes(home.path(), &digest, &bytes).unwrap();
        record.digest = format!("sha256:{digest}");
        record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
        gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
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
        let (home, mut record) = gents_pack::plugin::test_fixtures::installed_plugin(
            &crate::plugin::tests::executor::host_call_plugin_wat(&request, count),
            None,
        );
        record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
        gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
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

    /// `config plugin run` drives a host-call plugin through the tool's own
    /// invocation: the guest reaches the caller's tools only when the
    /// caller's Tools select it with `tool_calls: true`.
    #[tokio::test]
    async fn plugin_run_serves_a_host_call_plugin_under_the_callers_selection() {
        let (home, mut record) = gents_pack::plugin::test_fixtures::installed_plugin(
            &crate::plugin::tests::executor::host_call_plugin_wat(
                &json!({"tool_name": "query", "arguments": {}}),
                1,
            ),
            None,
        );
        record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
        gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
        let coordinate = format!("{}/{}", record.namespace, record.name);
        let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        )));
        for (tool_calls, expected) in [
            (true, json!({"result": {"ordinal": 1}})),
            (
                false,
                json!({"error": "tool access is disabled for this invocation"}),
            ),
        ] {
            let selected = [gents_documents::PluginToolRef {
                plugin: coordinate.clone(),
                digest: None,
                tool_calls,
                input_fields: Vec::new(),
                job: None,
            }];
            let tool =
                crate::plugin::tool::PluginTool::for_run(executor.clone(), &coordinate, &selected)
                    .unwrap();
            let (output, unserved, _) =
                gents_loop::tool_effects::scope(Some(Arc::new(Tools)), tool.run_once(json!({})))
                    .await
                    .unwrap();
            assert_eq!(output, expected, "tool_calls {tool_calls}");
            assert_eq!(unserved, None);
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
        let cases = &gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases["tool_calls"]
            ["host_call_lifetime"];
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
    fn compute_allowances_conform_to_lean() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases["tool_calls"]
            ["compute_allowance"];
        for case in cases.as_array().unwrap() {
            let actual = compute_allowance(
                Duration::from_secs(case["declared"].as_u64().unwrap()),
                Duration::from_secs(case["remaining"].as_u64().unwrap()),
                case["stopped"].as_bool().unwrap(),
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
        let cases = &gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases["tool_calls"]
            ["sleep"];
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
