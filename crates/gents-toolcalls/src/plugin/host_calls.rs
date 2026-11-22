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

#[cfg(any(test, feature = "test-fixtures"))]
pub mod live {
    //! Which guests still occupy a worker thread, by artifact digest, with
    //! the compute allowance each was given. Waiters subscribe to a watch,
    //! which checks the current value first, so no start or release is missed.
    use std::sync::LazyLock;
    use std::time::Duration;

    use tokio::sync::watch;

    type Guests = Vec<(String, Duration)>;

    static GUESTS: LazyLock<watch::Sender<Guests>> =
        LazyLock::new(|| watch::Sender::new(Vec::new()));

    pub(super) struct Guard(String);

    impl Guard {
        pub fn new(digest: String, allowance: Duration) -> Self {
            GUESTS.send_modify(|guests| guests.push((digest.clone(), allowance)));
            Self(digest)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            GUESTS.send_modify(|guests| {
                if let Some(index) = guests.iter().position(|(digest, _)| digest == &self.0) {
                    guests.swap_remove(index);
                }
            });
        }
    }

    fn allowances(guests: &Guests, digest: &str) -> Vec<Duration> {
        guests
            .iter()
            .filter(|(guest, _)| guest == digest)
            .map(|(_, allowance)| *allowance)
            .collect()
    }

    /// The allowances of the guests of `digest`, once one occupies a worker.
    pub async fn started(digest: &str) -> Vec<Duration> {
        let mut guests = GUESTS.subscribe();
        let guests = guests
            .wait_for(|guests| !allowances(guests, digest).is_empty())
            .await
            .expect("the registry's sender is static");
        allowances(&guests, digest)
    }

    /// Resolves once no guest of `digest` occupies a worker thread.
    pub async fn released(digest: &str) {
        GUESTS
            .subscribe()
            .wait_for(|guests| allowances(guests, digest).is_empty())
            .await
            .expect("the registry's sender is static");
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
enum Effect {
    Tool(ToolRequest),
    Sleep(SleepRequest),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DescribeRequest {
    describe: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRequest {
    batch: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Request {
    Effect(Effect),
    Describe(DescribeRequest),
    Batch(BatchRequest),
}

const EFFECT_SHAPES: &str = "{tool_name, arguments} or {sleep_ms}";

fn tool_access_disabled() -> Value {
    json!({"error": "tool access is disabled for this invocation"})
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
pub fn budget_for_invocation(
    call: &gents_loop::tool::ToolCallEnvelope,
    mut budget: PluginBudget,
) -> PluginBudget {
    if let Some(context) = call.runtime() {
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

async fn perform(
    effect: Effect,
    tools: &mut Option<tool_calls::Session>,
    deadline: tokio::time::Instant,
) -> Result<Value, String> {
    match effect {
        Effect::Tool(request) => match tools {
            Some(tools) => tools.call(&request.tool_name, request.arguments).await,
            None => Ok(tool_access_disabled()),
        },
        Effect::Sleep(request) => {
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
    }
}

async fn describe(name: Value, tools: &mut Option<tool_calls::Session>) -> Value {
    match (tools, name) {
        (None, _) => tool_access_disabled(),
        (Some(tools), Value::Null) => tools.describe(None).await,
        (Some(tools), Value::String(name)) => tools.describe(Some(&name)).await,
        (Some(_), _) => json!({"error": "describe takes a tool name, or null for every name"}),
    }
}

async fn perform_tools(
    requests: Vec<(String, Map<String, Value>)>,
    tools: &mut Option<tool_calls::Session>,
) -> Result<Vec<Value>, String> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    match tools {
        Some(tools) => tools.call_batch(requests).await,
        None => Ok(requests.iter().map(|_| tool_access_disabled()).collect()),
    }
}

/// A batch is its members in request order, and each answer takes its
/// member's position. Consecutive tool members are dispatched together; a
/// sleep or a malformed member is performed alone, after them.
async fn serve(
    bytes: &[u8],
    tools: &mut Option<tool_calls::Session>,
    deadline: tokio::time::Instant,
) -> Result<Value, String> {
    match serde_json::from_slice::<Request>(bytes) {
        Ok(Request::Effect(effect)) => perform(effect, tools, deadline).await,
        Ok(Request::Describe(request)) => Ok(describe(request.describe, tools).await),
        Ok(Request::Batch(request)) => {
            let mut results = Vec::with_capacity(request.batch.len());
            let mut pending = Vec::new();
            for member in request.batch {
                match serde_json::from_value::<Effect>(member) {
                    Ok(Effect::Tool(request)) => {
                        pending.push((request.tool_name, request.arguments));
                    }
                    other => {
                        results.extend(perform_tools(std::mem::take(&mut pending), tools).await?);
                        results.push(match other {
                            Ok(effect) => perform(effect, tools, deadline).await?,
                            Err(error) => {
                                json!({"error": format!("expected {EFFECT_SHAPES}: {error}")})
                            }
                        });
                    }
                }
            }
            results.extend(perform_tools(pending, tools).await?);
            Ok(json!({ "results": results }))
        }
        Err(error) => Ok(
            json!({"error": format!("expected {EFFECT_SHAPES}, {{describe}} or {{batch}}: {error}")}),
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
        #[cfg(any(test, feature = "test-fixtures"))]
        let _live = live::Guard::new(runner.artifact_digest(), guest_budget.wall_clock);
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
mod script_e2e;

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
        let context = gents_loop::tool::ToolCallEnvelope::default()
            .with_effects(Some(Arc::new(Tools) as Arc<dyn ToolEffectDispatcher>));
        let call = executor.call_data_bound_for(
&record, json!({"script": "let sum=0; for(let i=0;i<3;i++) sum += (await tools.query({})).ordinal; await sleep(1); return {sum};"}), crate::plugin::executor::ToolInvocation { tool_root: None, own_folder: None, tool_calls: true, context: &context }, None);
        let result = call.await.unwrap();
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
                crate::plugin::executor::ToolInvocation {
                    tool_root: None,
                    own_folder: None,
                    tool_calls: false,
                    context: &gents_loop::tool::ToolCallEnvelope::default(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.output, "undefined");
    }

    impl ToolEffectDispatcher for Tools {
        fn call<'a>(
            &'a self,
            _parent: &'a gents_loop::tool::ToolCallEnvelope,
            ordinal: u32,
            name: &'a str,
            arguments: &'a str,
        ) -> gents_loop::tool::BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
            Box::pin(async move {
                assert_eq!(name, "query");
                assert_eq!(arguments, "{}");
                Ok(ToolOutcome::Completed(
                    json!({"ordinal": ordinal}).to_string(),
                ))
            })
        }

        fn definitions<'a>(
            &'a self,
        ) -> gents_loop::tool::BoxFuture<'a, Vec<gents_loop::tool::ToolDefinition>> {
            Box::pin(async {
                vec![gents_loop::tool::ToolDefinition {
                    name: "query".into(),
                    description: "Reads rows.".into(),
                    parameters: json!({"type": "object"}),
                }]
            })
        }
    }

    async fn execute(request: Value, count: u32, grant: bool, input: Value) -> PluginOutcome {
        execute_in(None, request, count, grant, input).await
    }

    async fn execute_in(
        runtime: Option<gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext>,
        request: Value,
        count: u32,
        grant: bool,
        input: Value,
    ) -> PluginOutcome {
        let (home, mut record) = gents_pack::plugin::test_fixtures::installed_plugin(
            &crate::plugin::tests::executor::host_call_plugin_wat(&request, count),
            None,
        );
        record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
        gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
        let executor = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
        let context = gents_loop::tool::ToolCallEnvelope::new(runtime)
            .with_effects(Some(Arc::new(Tools) as Arc<dyn ToolEffectDispatcher>));
        executor
            .call_data_bound_for(
                &record,
                input,
                crate::plugin::executor::ToolInvocation {
                    tool_root: None,
                    own_folder: None,
                    tool_calls: grant,
                    context: &context,
                },
                None,
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
            let context = gents_loop::tool::ToolCallEnvelope::default()
                .with_effects(Some(Arc::new(Tools) as Arc<dyn ToolEffectDispatcher>));
            let (output, unserved, _) = tool.run_once(json!({}), &context).await.unwrap();
            assert_eq!(output, expected, "tool_calls {tool_calls}");
            assert_eq!(unserved, None);
        }
    }

    #[tokio::test]
    async fn a_batch_answers_each_member_in_its_position() {
        let outcome = execute(
            json!({"batch": [
                {"tool_name": "query", "arguments": {}},
                {"sleep_ms": 1},
                {"tool_name": "query"},
                {"tool_name": "query", "arguments": {}},
            ]}),
            1,
            true,
            json!({}),
        )
        .await;
        let results = outcome.output["results"].as_array().unwrap();
        assert_eq!(results[0], json!({"result": {"ordinal": 1}}));
        assert_eq!(results[1], json!({"result": null}));
        assert!(results[2]["error"]
            .as_str()
            .unwrap()
            .starts_with("expected {tool_name, arguments} or {sleep_ms}"));
        assert_eq!(results[3], json!({"result": {"ordinal": 2}}));
        assert_eq!(results.len(), 4);
    }

    struct BatchOnly;

    impl ToolEffectDispatcher for BatchOnly {
        fn call<'a>(
            &'a self,
            _: &'a gents_loop::tool::ToolCallEnvelope,
            _: u32,
            _: &'a str,
            _: &'a str,
        ) -> gents_loop::tool::BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
            Box::pin(async { panic!("the host split a batch into single calls") })
        }

        fn batch<'a>(
            &'a self,
            _: &'a gents_loop::tool::ToolCallEnvelope,
            calls: Vec<gents_loop::tool_effects::ToolEffectCall>,
        ) -> gents_loop::tool::BoxFuture<'a, Vec<gents_loop::tool_effects::EffectResult>> {
            Box::pin(async move {
                calls
                    .into_iter()
                    .map(|call| {
                        Ok(ToolOutcome::Completed(
                            json!({"ordinal": call.ordinal, "name": call.name}).to_string(),
                        ))
                    })
                    .collect()
            })
        }

        fn definitions<'a>(
            &'a self,
        ) -> gents_loop::tool::BoxFuture<'a, Vec<gents_loop::tool::ToolDefinition>> {
            Box::pin(async { Vec::new() })
        }
    }

    #[tokio::test]
    async fn consecutive_tool_members_reach_the_dispatcher_as_one_batch() {
        let parent = gents_loop::tool::ToolCallEnvelope::default()
            .with_effects(Some(Arc::new(BatchOnly) as Arc<dyn ToolEffectDispatcher>));
        let mut tools = tool_calls::Session::for_call(true, &parent);
        let request = json!({"batch": [
            {"tool_name": "a", "arguments": {}},
            {"tool_name": "b", "arguments": {}},
            {"sleep_ms": 1},
            {"tool_name": "c", "arguments": {}},
        ]});
        let answer = serve(
            &serde_json::to_vec(&request).unwrap(),
            &mut tools,
            tokio::time::Instant::now() + Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert_eq!(
            answer,
            json!({"results": [
                {"result": {"ordinal": 1, "name": "a"}},
                {"result": {"ordinal": 2, "name": "b"}},
                {"result": null},
                {"result": {"ordinal": 3, "name": "c"}},
            ]})
        );
    }

    #[tokio::test]
    async fn a_guest_lists_and_describes_its_surface() {
        let outcome = execute(json!({"describe": null}), 1, true, json!({})).await;
        assert_eq!(outcome.output, json!({"result": ["query"]}));
        let outcome = execute(json!({"describe": "query"}), 1, true, json!({})).await;
        assert_eq!(
            outcome.output,
            json!({"result": {"name": "query", "description": "Reads rows.",
                "parameters": {"type": "object"}}})
        );
        let outcome = execute(json!({"describe": 1}), 1, true, json!({})).await;
        assert!(outcome.output["error"]
            .as_str()
            .unwrap()
            .starts_with("describe takes a tool name"));
    }

    #[tokio::test]
    async fn describing_conforms_to_lean() {
        for case in gents_lean_vocab::families::tool_policy_plugin_calls_describable::cases() {
            let name = if case.input.selected {
                "query"
            } else {
                "absent"
            };
            let outcome = execute(
                json!({ "describe": name }),
                1,
                case.input.tool_access,
                json!({}),
            )
            .await;
            case.check(outcome.output.get("result").is_some());
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
        let deadline = Some(chrono::Utc::now() + chrono::Duration::seconds(30));
        let token = tokio_util::sync::CancellationToken::new();
        let runtime = gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext::entered(
            None,
            gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry {
                identity: None,
                deadline_at: deadline,
                cancellation_token: token.clone(),
                workspace_cwd: None,
                live_output: None,
                session_id: None,
                correlation: None,
                source_fields: Default::default(),
                background: true,
            },
        );
        let outcome = execute_in(
            Some(runtime),
            json!({"sleep_ms": 6000}),
            1,
            false,
            json!({}),
        )
        .await;
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
        gents_lean_vocab::families::plugin_resources_host_call_lifetimes::replay(|i| {
            gents_lean_vocab::families::plugin_resources_host_call_lifetimes::LifetimeOutput {
                host_call_lifetime: wall_clock_for_invocation(
                    Duration::from_secs(i.declared),
                    i.background,
                    i.remaining.map(Duration::from_secs),
                )
                .as_secs(),
            }
        });
    }

    #[test]
    fn compute_allowances_conform_to_lean() {
        gents_lean_vocab::families::plugin_resources_compute_allowances::replay(|i| {
            gents_lean_vocab::families::plugin_resources_compute_allowances::AllowanceOutput {
                compute_allowance: compute_allowance(
                    Duration::from_secs(i.declared),
                    Duration::from_secs(i.remaining),
                    i.stopped,
                )
                .as_secs(),
            }
        });
    }

    #[test]
    fn sleep_bounds_conform_to_lean() {
        gents_lean_vocab::families::plugin_resources_sleeps::replay(|i| {
            gents_lean_vocab::families::plugin_resources_sleeps::SleepOutput {
                sleep: sleep_admissible(i.milliseconds, Duration::from_millis(i.remaining)),
            }
        });
    }
}
