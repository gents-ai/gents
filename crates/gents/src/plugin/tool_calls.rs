//! Host-mediated calls to the installed plugin's selected tool surface.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};
use serde::Deserialize;
use serde_json::{json, Map, Value};

pub(super) const MAX_EFFECTS: u32 = 64;
const MAX_RESULT_BYTES: usize = 1024 * 1024;

pub(super) fn available(granted: bool, parent: bool, nested: bool) -> bool {
    granted && parent && !nested
}

pub(crate) fn supported(name: &str) -> bool {
    !crate::toolset::is_session_message_tool(name)
        && !matches!(
            name,
            crate::goal::UPDATE_GOAL_TOOL_NAME
                | crate::toolset::SPAWN_PROCESS_TOOL_NAME
                | crate::toolset::WAIT_PROCESS_TOOL_NAME
        )
}

fn reserve(used: u32) -> Option<u32> {
    (used < MAX_EFFECTS).then(|| used + 1)
}

fn result_fits(bytes: usize) -> bool {
    bytes <= MAX_RESULT_BYTES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    id: String,
    tool_name: String,
    arguments: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Batch {
    pub(super) requests: Vec<Request>,
    pub(super) state: Option<Value>,
}

pub(super) fn parse_batch(output: &Value) -> Result<Option<Batch>, String> {
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

pub(super) struct Session {
    dispatcher: Arc<dyn ToolEffectDispatcher>,
    used: u32,
    pub(super) fatal: Option<String>,
}

impl Session {
    pub(super) fn for_call(granted: bool) -> Option<Self> {
        let dispatcher = gents_loop::tool_effects::current();
        if !available(granted, dispatcher.is_some(), false) {
            return None;
        }
        Some(Self {
            dispatcher: dispatcher?,
            used: 0,
            fatal: None,
        })
    }

    pub(super) async fn serve(
        &mut self,
        requests: Vec<Request>,
        deadline: Instant,
    ) -> Map<String, Value> {
        let mut results = Map::new();
        for request in requests {
            let result = match reserve(self.used) {
                None => json!({"error": format!("the plugin used its {MAX_EFFECTS} tool calls")}),
                Some(next) => {
                    self.used = next;
                    if Instant::now() >= deadline {
                        json!({"error": "the plugin's tool-call deadline expired"})
                    } else {
                        let arguments = Value::Object(request.arguments).to_string();
                        // A child may itself be an installed plugin, but it receives
                        // no delegation service and cannot recursively call tools.
                        let outcome = gents_loop::tool_effects::scope(
                            None,
                            self.dispatcher.call(
                                next,
                                &request.tool_name,
                                &arguments,
                                deadline.saturating_duration_since(Instant::now()),
                            ),
                        )
                        .await;
                        match outcome {
                            Ok(outcome) => {
                                let text = outcome.model_facing_text();
                                if !result_fits(text.len()) {
                                    json!({"error": "the tool result exceeds 1 MiB; request a smaller result"})
                                } else if matches!(
                                    outcome,
                                    crate::tool_call_lifecycle::ToolOutcome::Completed(..)
                                ) {
                                    json!({"result": serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text.to_owned()))})
                                } else {
                                    let error = match &outcome {
                                        crate::tool_call_lifecycle::ToolOutcome::Cancelled => {
                                            "the tool call was cancelled"
                                        }
                                        crate::tool_call_lifecycle::ToolOutcome::TimedOut {
                                            ..
                                        } => "the tool call timed out",
                                        _ => text,
                                    };
                                    json!({"error": error})
                                }
                            }
                            Err(ToolEffectError::Unavailable(error)) => json!({"error": error}),
                            Err(ToolEffectError::Fatal(error)) => {
                                self.fatal = Some(error);
                                break;
                            }
                        }
                    }
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
    use crate::plugin::{rounds, PluginBudget, PluginOutcome, PluginVerdict};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Tools(Mutex<Vec<(u32, String, Value)>>);

    impl ToolEffectDispatcher for Tools {
        fn call<'a>(
            &'a self,
            ordinal: u32,
            name: &'a str,
            arguments: &'a str,
            _budget: std::time::Duration,
        ) -> gents_loop::tool::BoxFuture<
            'a,
            Result<crate::tool_call_lifecycle::ToolOutcome, ToolEffectError>,
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
                Ok(crate::tool_call_lifecycle::ToolOutcome::Completed(
                    result.to_string(),
                ))
            })
        }
    }

    #[tokio::test]
    async fn rounds_chain_a_query_result_into_a_chart_without_model_calls() {
        let tools = Arc::new(Tools::default());
        gents_loop::tool_effects::scope(Some(tools.clone()), async {
            let session = Session::for_call(true).unwrap();
            let round: rounds::Round = Arc::new(|input, _| {
                let output = match input.get("state").and_then(Value::as_str) {
                    None => json!({"tool_calls":{"requests":[{"id":"table","tool_name":"query","arguments":{"sql":"SELECT day, sales FROM sales"}}],"state":"chart"}}),
                    Some("chart") => json!({"tool_calls":{"requests":[{"id":"image","tool_name":"charts","arguments":{"data":input["tool_results"]["table"]["result"]}}],"state":"done"}}),
                    Some("done") => input["tool_results"]["image"]["result"].clone(),
                    _ => unreachable!(),
                };
                Ok(PluginOutcome { verdict: PluginVerdict::Success, output, diagnostics: String::new(), fuel_used: 1, wall_ms: 0 })
            });
            let result = rounds::drive(rounds::HostCalls { model: None, http: None, tools: Some(session) },
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
        use crate::llm::tool::ToolDyn;
        use crate::plugin::tests::executor::{asking_plugin_wat, installed_plugin};
        let request =
            json!({"tool_calls":{"requests":[{"id":"table","tool_name":"query","arguments":{}}]}});
        let wat = asking_plugin_wat("tool_calls", "tool_results", &request, false);
        let (home, _) = installed_plugin(&wat, None);
        let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        )));
        let tools = Arc::new(Tools::default());
        for granted in [false, true] {
            let selected = crate::document_config::PluginToolRef {
                plugin: "team/plugin".into(),
                digest: None,
                input_fields: vec![],
                tool_calls: granted,
            };
            let plugin =
                crate::plugin::tool::PluginTool::resolve(executor.clone(), &selected, None)
                    .unwrap();
            let output =
                gents_loop::tool_effects::scope(Some(tools.clone()), plugin.call("{}".into()))
                    .await
                    .unwrap();
            let output: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(output.get("tool_results").is_some(), granted);
        }
        assert_eq!(tools.0.lock().unwrap().len(), 1);
        assert!(Session::for_call(true).is_none());
    }

    #[tokio::test]
    async fn receipt_records_effective_tool_service_and_exact_first_guest_input() {
        use crate::llm::tool::ToolDyn;
        use crate::plugin::tests::executor::installed_echo;
        use sha2::{Digest, Sha256};

        let (home, record) = installed_echo();
        let executor = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        )));
        let tools = Arc::new(Tools::default());
        let input = json!({"value": 1, "state": "caller", "tool_calls": false,
            "tool_results": {"forged": true}});
        for (granted, live) in [(false, true), (true, false), (true, true)] {
            let selected = crate::document_config::PluginToolRef {
                plugin: "team/plugin".into(),
                digest: None,
                input_fields: vec![],
                tool_calls: granted,
            };
            let plugin =
                crate::plugin::tool::PluginTool::resolve(executor.clone(), &selected, None)
                    .unwrap();
            let dispatcher = live.then(|| tools.clone() as Arc<dyn ToolEffectDispatcher>);
            let call = gents_loop::tool_effects::scope(
                dispatcher,
                plugin.call_with_receipt(input.to_string()),
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
            let canonical = crate::workspace::canonical_json_string(&observed).unwrap();
            assert_eq!(
                receipt.input_digest,
                format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
            );
        }
        let call =
            gents_loop::tool_effects::scope(Some(tools.clone()), executor.call(&record, input))
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
                _: u32,
                _: &'a str,
                _: &'a str,
                _: std::time::Duration,
            ) -> gents_loop::tool::BoxFuture<
                'a,
                Result<crate::tool_call_lifecycle::ToolOutcome, ToolEffectError>,
            > {
                Box::pin(async {
                    Err(ToolEffectError::Fatal(
                        "durable child settlement failed".into(),
                    ))
                })
            }
        }
        gents_loop::tool_effects::scope(Some(Arc::new(Fatal)), async {
            let session = Session::for_call(true).unwrap();
            let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = polls.clone();
            let round: rounds::Round = Arc::new(move |_, _| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(PluginOutcome { verdict: PluginVerdict::Success,
                    output: json!({"tool_calls":{"requests":[{"id":"x","tool_name":"query","arguments":{}}]}}),
                    diagnostics: String::new(), fuel_used: 1, wall_ms: 0 })
            });
            let result = rounds::drive(rounds::HostCalls { model: None, http: None, tools: Some(session) },
                json!({}), PluginBudget::default(), round).await.unwrap();
            assert_ne!(result.verdict, PluginVerdict::Success);
            assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
        }).await;
    }

    #[test]
    fn generated_tool_call_bounds_and_grants() {
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases["tool_calls"];
        for case in cases["supported"].as_array().unwrap() {
            assert_eq!(
                supported(case["name"].as_str().unwrap()),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
        for case in cases["availability"].as_array().unwrap() {
            assert_eq!(
                available(
                    case["grant"].as_bool().unwrap(),
                    case["parent"].as_bool().unwrap(),
                    case["nested"].as_bool().unwrap()
                ),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
        for case in cases["reservation"].as_array().unwrap() {
            let expected = case["expected"].as_u64().map(|value| value as u32);
            assert_eq!(
                reserve(case["used"].as_u64().unwrap() as u32),
                expected,
                "{case}"
            );
        }
        for case in cases["result_bytes"].as_array().unwrap() {
            assert_eq!(
                result_fits(case["bytes"].as_u64().unwrap() as usize),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
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
