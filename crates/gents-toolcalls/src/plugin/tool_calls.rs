use std::collections::BTreeSet;
use std::sync::Arc;

use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};
use gents_protocol::plugin_tool_call_limits::{MAX_EFFECTS, MAX_PAYLOAD_BYTES};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use gents_loop::tool_call_lifecycle::ToolOutcome;

pub fn available(granted: bool, live_dispatcher: bool) -> bool {
    granted && live_dispatcher
}

pub fn supported(_name: &str, _job_tools: &BTreeSet<String>) -> bool {
    true
}

pub fn composition_note() -> String {
    format!(
        "\nThis plugin may call up to {MAX_EFFECTS} of this request's tools in sequence, with their existing permissions and lifecycle behavior."
    )
}

fn reserve(used: u32) -> Option<u32> {
    (used < MAX_EFFECTS).then(|| used + 1)
}

fn result_fits(bytes: usize) -> bool {
    bytes <= MAX_PAYLOAD_BYTES
}

fn settle(outcome: Result<ToolOutcome, ToolEffectError>) -> Result<Value, String> {
    let error = match outcome {
        Err(ToolEffectError::Fatal(reason)) => return Err(reason),
        Err(ToolEffectError::Unavailable(error)) => error,
        Ok(outcome) => {
            let text = outcome.model_facing_text();
            if !result_fits(text.len()) {
                format!(
                    "the tool result exceeds {} MiB; request a smaller result",
                    MAX_PAYLOAD_BYTES >> 20
                )
            } else {
                match outcome {
                    ToolOutcome::Completed(text) => {
                        return Ok(json!({"result": serde_json::from_str::<Value>(&text)
                            .unwrap_or(Value::String(text))}));
                    }
                    ToolOutcome::Failed { text, .. } => text,
                    ToolOutcome::Cancelled => "the tool call was cancelled".to_owned(),
                    ToolOutcome::TimedOut { .. } => "the tool call timed out".to_owned(),
                }
            }
        }
    };
    Ok(json!({ "error": error }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    id: String,
    tool_name: String,
    arguments: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub requests: Vec<Request>,
    pub state: Option<Value>,
}

pub fn parse_batch(output: &Value) -> Result<Option<Batch>, String> {
    let Some(value) = output.get("tool_calls").and_then(Value::as_object) else {
        return Ok(None);
    };
    let batch: Batch = serde_json::from_value(Value::Object(value.clone())).map_err(|_| {
        "tool_calls needs requests [{id, tool_name, arguments}] and optional state".to_owned()
    })?;
    if batch.requests.is_empty() || batch.requests.len() > MAX_EFFECTS as usize {
        return Err(format!("tool_calls needs 1 to {MAX_EFFECTS} requests"));
    }
    let mut ids = BTreeSet::new();
    for request in &batch.requests {
        if request.id.is_empty() || request.tool_name.is_empty() || !ids.insert(request.id.as_str())
        {
            return Err(
                "tool_calls needs distinct nonempty ids and nonempty tool_name values".into(),
            );
        }
    }
    Ok(Some(batch))
}

pub struct Session {
    dispatcher: Arc<dyn ToolEffectDispatcher>,
    parent: gents_loop::tool::ToolCallEnvelope,
    used: u32,
    pub fatal: Option<String>,
}

impl Session {
    pub async fn call(
        &mut self,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        let Some(next) = reserve(self.used) else {
            return Ok(json!({"error": format!("the plugin used its {MAX_EFFECTS} tool calls")}));
        };
        self.used = next;
        let arguments = Value::Object(arguments).to_string();
        settle(
            gents_loop::tool_effects::scope(
                None,
                self.dispatcher.call(&self.parent, next, name, &arguments),
            )
            .await,
        )
    }

    /// The `tool_calls` service of the plugin call `parent`, if it is granted
    /// and the call may compose.
    pub fn for_call(granted: bool, parent: &gents_loop::tool::ToolCallEnvelope) -> Option<Self> {
        let dispatcher = gents_loop::tool_effects::current();
        if !available(granted, dispatcher.is_some()) {
            return None;
        }
        Some(Self {
            dispatcher: dispatcher?,
            parent: parent.clone(),
            used: 0,
            fatal: None,
        })
    }

    pub async fn serve(&mut self, requests: Vec<Request>) -> Map<String, Value> {
        let mut results = Map::new();
        for request in requests {
            let result = match self.call(&request.tool_name, request.arguments).await {
                Ok(result) => result,
                Err(reason) => {
                    self.fatal = Some(reason);
                    break;
                }
            };
            results.insert(request.id, result);
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::rounds;
    use gents_pack::{plugin::PluginBudget, plugin::PluginOutcome, plugin::PluginVerdict};
    use std::sync::Mutex;

    #[test]
    fn support_includes_lifecycle_and_job_tools() {
        let job_tools = BTreeSet::from(["workflow_run".to_owned()]);
        assert!(supported("workflow_run", &job_tools));
        assert!(supported("bash", &job_tools));
        assert!(supported("workflow_run", &BTreeSet::new()));
        assert!(supported(
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            &job_tools
        ));
        assert!(supported(
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            &BTreeSet::new()
        ));
    }

    #[derive(Default)]
    struct Tools(Mutex<Vec<(u32, String, Value)>>);

    impl ToolEffectDispatcher for Tools {
        fn call<'a>(
            &'a self,
            _parent: &'a gents_loop::tool::ToolCallEnvelope,
            ordinal: u32,
            name: &'a str,
            arguments: &'a str,
        ) -> gents_loop::tool::BoxFuture<
            'a,
            Result<gents_loop::tool_call_lifecycle::ToolOutcome, ToolEffectError>,
        > {
            Box::pin(async move {
                assert!(
                    gents_loop::tool_effects::current().is_none(),
                    "child inherited delegation"
                );
                let input: Value = serde_json::from_str(arguments).unwrap();
                self.0
                    .lock()
                    .unwrap()
                    .push((ordinal, name.to_owned(), input.clone()));
                let result = match name {
                    "query" => json!({"columns":["day","sales"],"rows":[["Mon",10],["Tue",20]]}),
                    "charts" => {
                        assert_eq!(
                            input["data"],
                            json!({"columns":["day","sales"],"rows":[["Mon",10],["Tue",20]]})
                        );
                        json!({"alt":"Sales by day","rendered":true})
                    }
                    _ => {
                        return Err(ToolEffectError::Unavailable(
                            "tool is not on this request's selected surface".into(),
                        ));
                    }
                };
                Ok(gents_loop::tool_call_lifecycle::ToolOutcome::Completed(
                    result.to_string(),
                ))
            })
        }
    }

    #[tokio::test]
    async fn rounds_chain_a_query_result_into_a_chart_without_model_calls() {
        let tools = Arc::new(Tools::default());
        gents_loop::tool_effects::scope(Some(tools.clone()), async {
            let session = Session::for_call(true, &gents_loop::tool::ToolCallEnvelope::ambient()).unwrap();
            let round: rounds::Round = Arc::new(|input, _| {
                let output = match input.get("state").and_then(Value::as_str) {
                    None => json!({"tool_calls":{"requests":[{"id":"table","tool_name":"query","arguments":{"sql":"SELECT day, sales FROM sales"}}],"state":"chart"}}),
                    Some("chart") => json!({"tool_calls":{"requests":[{"id":"image","tool_name":"charts","arguments":{"data":input["tool_results"]["table"]["result"]}}],"state":"done"}}),
                    Some("done") => input["tool_results"]["image"]["result"].clone(),
                    _ => unreachable!(),
                };
                Ok(PluginOutcome { verdict: PluginVerdict::Success, output, diagnostics: String::new(), fuel_used: 1, wall_ms: 0 })
            });
            let result = rounds::drive(rounds::HostCalls { model: None, http: None, tools: Some(session), data: None, jobs: None },
                json!({"state":"forged", "tool_results":{"table":{"result":"forged"}}}), PluginBudget::default(), round).await.unwrap();
            assert_eq!(result.output, json!({"alt":"Sales by day","rendered":true}));
            assert_eq!(result.fuel_used, 3);
        }).await;
        let calls = tools.0.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(n, name, _)| (*n, name.as_str()))
                .collect::<Vec<_>>(),
            [(1, "query"), (2, "charts")]
        );
    }

    #[tokio::test]
    async fn an_installed_wasm_plugin_gets_only_explicitly_granted_tool_calls() {
        use gents_loop::tool::ToolDyn;
        use gents_pack::plugin::test_fixtures::{asking_plugin_wat, installed_plugin};
        let request =
            json!({"tool_calls":{"requests":[{"id":"table","tool_name":"query","arguments":{}}]}});
        let wat = asking_plugin_wat("tool_calls", "tool_results", &request, false);
        let (home, _) = installed_plugin(&wat, None);
        let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        )));
        let tools = Arc::new(Tools::default());
        for granted in [false, true] {
            let selected = gents_documents::PluginToolRef {
                plugin: "team/plugin".into(),
                digest: None,
                input_fields: vec![],
                job: None,
                tool_calls: granted,
            };
            let plugin =
                crate::plugin::tool::PluginTool::resolve(executor.clone(), &selected, None)
                    .unwrap();
            let output = gents_loop::tool_effects::scope(
                Some(tools.clone()),
                plugin.call("{}".into(), &gents_loop::tool::ToolCallEnvelope::ambient()),
            )
            .await
            .unwrap();
            let output: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(output.get("tool_results").is_some(), granted);
        }
        assert_eq!(tools.0.lock().unwrap().len(), 1);
        assert!(Session::for_call(true, &gents_loop::tool::ToolCallEnvelope::ambient()).is_none());
    }

    #[tokio::test]
    async fn receipt_records_effective_tool_service_and_exact_first_guest_input() {
        use gents_loop::tool::ToolDyn;
        use gents_pack::plugin::test_fixtures::installed_echo;
        use sha2::{Digest, Sha256};

        let (home, record) = installed_echo();
        let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        )));
        let tools = Arc::new(Tools::default());
        let input = json!({"value": 1, "state": "caller", "tool_calls": false,
            "tool_results": {"forged": true}});
        for (granted, live) in [(false, true), (true, false), (true, true)] {
            let selected = gents_documents::PluginToolRef {
                plugin: "team/plugin".into(),
                digest: None,
                input_fields: vec![],
                job: None,
                tool_calls: granted,
            };
            let plugin =
                crate::plugin::tool::PluginTool::resolve(executor.clone(), &selected, None)
                    .unwrap();
            let dispatcher = live.then(|| tools.clone() as Arc<dyn ToolEffectDispatcher>);
            let call = gents_loop::tool_effects::scope(
                dispatcher,
                plugin.call_with_receipt(
                    input.to_string(),
                    &gents_loop::tool::ToolCallEnvelope::ambient(),
                ),
            )
            .await;
            let observed: Value = serde_json::from_str(&call.result.unwrap()).unwrap();
            let expected = if granted && live {
                json!({"value": 1, "tool_calls": true})
            } else {
                input.clone()
            };
            assert_eq!(observed, expected);
            let receipt = call.plugin_receipt.unwrap();
            assert_eq!(receipt.authority.unwrap().host_tools, granted && live);
            let canonical = gents_session::workspace::canonical_json_string(&observed).unwrap();
            assert_eq!(
                receipt.input_digest,
                format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
            );
        }
        let call = gents_loop::tool_effects::scope(
            Some(tools.clone()),
            executor.call(
                &record,
                input,
                &gents_loop::tool::ToolCallEnvelope::ambient(),
            ),
        )
        .await
        .unwrap();
        assert!(!call.receipt.authority.unwrap().host_tools);
        assert!(tools.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_fatal_hook_termination_cannot_be_swallowed_by_the_guest() {
        struct Fatal;
        impl ToolEffectDispatcher for Fatal {
            fn call<'a>(
                &'a self,
                _parent: &'a gents_loop::tool::ToolCallEnvelope,
                _: u32,
                _: &'a str,
                _: &'a str,
            ) -> gents_loop::tool::BoxFuture<
                'a,
                Result<gents_loop::tool_call_lifecycle::ToolOutcome, ToolEffectError>,
            > {
                Box::pin(async {
                    Err(ToolEffectError::Fatal(
                        "durable child settlement failed".into(),
                    ))
                })
            }
        }
        gents_loop::tool_effects::scope(Some(Arc::new(Fatal)), async {
            let session = Session::for_call(true, &gents_loop::tool::ToolCallEnvelope::ambient()).unwrap();
            let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = polls.clone();
            let round: rounds::Round = Arc::new(move |_, _| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(PluginOutcome { verdict: PluginVerdict::Success,
                    output: json!({"tool_calls":{"requests":[{"id":"x","tool_name":"query","arguments":{}}]}}),
                    diagnostics: String::new(), fuel_used: 1, wall_ms: 0 })
            });
            let result = rounds::drive(rounds::HostCalls { model: None, http: None, tools: Some(session), data: None, jobs: None },
                json!({}), PluginBudget::default(), round).await.unwrap();
            assert_ne!(result.verdict, PluginVerdict::Success);
            assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
        }).await;
    }

    #[test]
    fn supported_routes_conform_to_lean() {
        gents_lean_vocab::families::plugin_resources_supported_routes::replay::<String>(|i| {
            let names: Vec<&str> = match i.route.as_str() {
                "hook_owned" => crate::call::hook::persistence::HookOwnedTool::ROUTES
                    .iter()
                    .map(|(name, _)| *name)
                    .collect(),
                "ordinary" => vec!["bash", "query", "charts", "call_tool", "configure"],
                route => panic!("unknown route {route}"),
            };
            let observed: BTreeSet<bool> = names
                .into_iter()
                .map(|name| supported(name, &BTreeSet::new()))
                .collect();
            assert_eq!(observed.len(), 1, "route {} splits its tools", i.route);
            gents_lean_vocab::families::plugin_resources_supported_routes::SupportedOutput {
                supported: observed.into_iter().all(|value| value),
            }
        });
    }

    #[tokio::test]
    async fn tool_call_availability_conforms_to_lean() {
        for case in gents_lean_vocab::families::plugin_resources_availabilities::cases() {
            let granted = case.input.grant;
            let live: Option<Arc<dyn ToolEffectDispatcher>> = match case.input.dispatcher.as_str() {
                "live" | "child" => Some(Arc::new(Tools::default())),
                "absent" => None,
                dispatcher => panic!("unknown dispatcher scope {dispatcher}"),
            };
            let child = case.input.dispatcher == "child";
            let availability = gents_loop::tool_effects::scope(live, async {
                if child {
                    gents_loop::tool_effects::scope(None, async {
                        Session::for_call(granted, &gents_loop::tool::ToolCallEnvelope::ambient())
                            .is_some()
                    })
                    .await
                } else {
                    Session::for_call(granted, &gents_loop::tool::ToolCallEnvelope::ambient())
                        .is_some()
                }
            })
            .await;
            case.check(
                gents_lean_vocab::families::plugin_resources_availabilities::AvailabilityOutput {
                    availability,
                },
            );
        }
    }

    #[test]
    fn reservations_conform_to_lean() {
        gents_lean_vocab::families::plugin_resources_reservations::replay(|i| {
            gents_lean_vocab::families::plugin_resources_reservations::ReservationOutput {
                reservation: reserve(u32::try_from(i.used).unwrap()).map(u64::from),
            }
        });
    }

    #[test]
    fn result_sizes_conform_to_lean() {
        gents_lean_vocab::families::plugin_resources_result_sizes::replay(|i| {
            gents_lean_vocab::families::plugin_resources_result_sizes::BytesOutput {
                result_bytes: result_fits(usize::try_from(i.bytes).unwrap()),
            }
        });
    }

    #[test]
    fn child_settlements_conform_to_lean() {
        gents_lean_vocab::families::plugin_resources_settlements::replay::<String, String>(|i| {
            let outcome = match i.outcome.as_str() {
                "completed" => Ok(ToolOutcome::Completed("{}".into())),
                "failed" => Ok(ToolOutcome::Failed {
                    class: gents_loop::tool_call_lifecycle::FailureClass::External,
                    denial: None,
                    text: "boom".into(),
                }),
                "unavailable" => Err(ToolEffectError::Unavailable("unselected".into())),
                "timed_out" => Ok(ToolOutcome::TimedOut { deadline_at: None }),
                "cancelled" => Ok(ToolOutcome::Cancelled),
                "fatal" => Err(ToolEffectError::Fatal("durable failure".into())),
                outcome => panic!("unknown outcome {outcome}"),
            };
            let settlement = match settle(outcome) {
                Err(_) => "parent_fatal",
                Ok(value) if value.get("result").is_some() => "result",
                Ok(value) => {
                    assert!(value["error"].is_string(), "{}", i.outcome);
                    "guest_error"
                }
            };
            gents_lean_vocab::families::plugin_resources_settlements::SettlementOutput {
                settlement: settlement.to_owned(),
            }
        });
    }

    #[derive(Clone)]
    struct RoundClock(Arc<Mutex<std::time::Instant>>);

    impl RoundClock {
        pub fn new() -> Self {
            Self(Arc::new(Mutex::new(std::time::Instant::now())))
        }

        fn now(&self) -> std::time::Instant {
            *self.0.lock().unwrap()
        }

        fn advance(&self, milliseconds: u64) {
            *self.0.lock().unwrap() += std::time::Duration::from_millis(milliseconds);
        }
    }

    #[tokio::test]
    async fn serving_children_is_not_charged_to_the_guest_clock() {
        struct Slow(RoundClock);
        impl ToolEffectDispatcher for Slow {
            fn call<'a>(
                &'a self,
                _parent: &'a gents_loop::tool::ToolCallEnvelope,
                _: u32,
                _: &'a str,
                _: &'a str,
            ) -> gents_loop::tool::BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
                let clock = self.0.clone();
                Box::pin(async move {
                    clock.advance(400);
                    Ok(ToolOutcome::Completed("\"slow\"".into()))
                })
            }
        }
        let clock = RoundClock::new();
        gents_loop::tool_effects::scope(Some(Arc::new(Slow(clock.clone()))), async {
            let session =
                Session::for_call(true, &gents_loop::tool::ToolCallEnvelope::ambient()).unwrap();
            let guest_clock = clock.clone();
            let round: rounds::Round = Arc::new(move |input, _| {
                guest_clock.advance(10);
                let output = match input.get("state") {
                    None => json!({"tool_calls":{"requests":[
                        {"id":"a","tool_name":"slow","arguments":{}},
                        {"id":"b","tool_name":"slow","arguments":{}}],"state":"done"}}),
                    Some(_) => json!([input["tool_results"]["a"], input["tool_results"]["b"]]),
                };
                Ok(PluginOutcome {
                    verdict: PluginVerdict::Success,
                    output,
                    diagnostics: String::new(),
                    fuel_used: 1,
                    wall_ms: 0,
                })
            });
            let budget = PluginBudget {
                wall_clock: std::time::Duration::from_millis(200),
                ..PluginBudget::default()
            };
            let result = rounds::drive_with_clock(
                rounds::HostCalls {
                    model: None,
                    http: None,
                    tools: Some(session),
                    data: None,
                    jobs: None,
                },
                json!({}),
                budget,
                round,
                || clock.now(),
            )
            .await
            .unwrap();
            assert_eq!(
                result.verdict,
                PluginVerdict::Success,
                "{}",
                result.diagnostics
            );
            assert_eq!(result.output, json!([{"result":"slow"},{"result":"slow"}]));
        })
        .await;
    }

    #[tokio::test]
    async fn a_child_parked_on_operator_consent_is_not_charged_to_the_guest_clock() {
        struct Asking(std::path::PathBuf, std::path::PathBuf, RoundClock);
        impl ToolEffectDispatcher for Asking {
            fn call<'a>(
                &'a self,
                _parent: &'a gents_loop::tool::ToolCallEnvelope,
                _: u32,
                _: &'a str,
                _: &'a str,
            ) -> gents_loop::tool::BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
                let home = self.0.clone();
                let target = self.1.clone();
                let clock = self.2.clone();
                Box::pin(async move {
                    let resolved = gents_pack::plugin::allowed::resolve(
                        &target.display().to_string(),
                        None,
                        None,
                        &home,
                    )
                    .unwrap();
                    let request = gents_tools::plugin::approval::Request::new(
                        "team/plugin",
                        &resolved,
                        gents_pack::pack::BindAccess::Read,
                        None,
                    );
                    clock.advance(400);
                    let allowed = gents_tools::plugin::approval::ask(
                        &home,
                        &request,
                        gents_tools::plugin::approval::WAIT,
                    )
                    .await
                    .unwrap();
                    assert!(allowed, "the operator allowed the child's path");
                    Ok(ToolOutcome::Completed("\"allowed\"".into()))
                })
            }
        }
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("in.json");
        std::fs::write(&target, "{}").unwrap();
        let operator = gents_tools::plugin::approval::testing::Operator::answer(
            home.path(),
            gents_tools::plugin::approval::Answer::Once,
        )
        .await
        .unwrap();
        let clock = RoundClock::new();
        gents_loop::tool_effects::scope(
            Some(Arc::new(Asking(
                home.path().to_owned(),
                target,
                clock.clone(),
            ))),
            async {
                let session =
                    Session::for_call(true, &gents_loop::tool::ToolCallEnvelope::ambient())
                        .unwrap();
                let guest_clock = clock.clone();
                let round: rounds::Round = Arc::new(move |input, _| {
                    guest_clock.advance(10);
                    let output = match input.get("state") {
                        None => json!({"tool_calls":{"requests":[
                            {"id":"gate","tool_name":"asking","arguments":{}}],"state":"done"}}),
                        Some(_) => input["tool_results"]["gate"].clone(),
                    };
                    Ok(PluginOutcome {
                        verdict: PluginVerdict::Success,
                        output,
                        diagnostics: String::new(),
                        fuel_used: 1,
                        wall_ms: 0,
                    })
                });
                let budget = PluginBudget {
                    wall_clock: std::time::Duration::from_millis(200),
                    ..PluginBudget::default()
                };
                let result = rounds::drive_with_clock(
                    rounds::HostCalls {
                        model: None,
                        http: None,
                        tools: Some(session),
                        data: None,
                        jobs: None,
                    },
                    json!({}),
                    budget,
                    round,
                    || clock.now(),
                )
                .await
                .unwrap();
                assert_eq!(
                    result.verdict,
                    PluginVerdict::Success,
                    "{}",
                    result.diagnostics
                );
                assert_eq!(result.output, json!({"result":"allowed"}));
            },
        )
        .await;
        let prompt = operator.await.unwrap();
        assert!(prompt.contains("team/plugin"), "{prompt}");
        assert!(
            gents_tools::plugin::approval::pending(home.path())
                .unwrap()
                .is_empty(),
            "the answered question is removed from the queue"
        );
    }

    #[test]
    fn batches_require_unique_ids_and_object_arguments() {
        assert!(parse_batch(
            &json!({"tool_calls":{"requests":[{"id":"a","tool_name":"read","arguments":{}}]}})
        )
        .unwrap()
        .is_some());
        for requests in [
            json!([]),
            json!([{"id":"a","tool_name":"read","arguments":[]}]),
            json!([{"id":"a","tool_name":"read","arguments":{}},{"id":"a","tool_name":"read","arguments":{}}]),
        ] {
            assert!(parse_batch(&json!({"tool_calls":{"requests":requests}})).is_err());
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::available;

    #[test]
    fn plugin_call_availability_matches_the_lean_model() {
        gents_lean_vocab::families::tool_policy_plugin_calls_available::replay(|i| {
            available(i.selected_grant, i.live_dispatcher)
        });
    }
}
