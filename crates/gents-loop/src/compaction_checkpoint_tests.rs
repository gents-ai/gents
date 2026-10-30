use std::sync::{Arc, Mutex};

use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use serde_json::{json, Value};

use super::*;
use crate::backend_provider::BackendProviderKind;
use crate::claude_messages_body::{build_messages_body, ADVERTISED_REASONING_EFFORTS_PARAM};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use gents_protocol::message::Message;

#[derive(Clone)]
struct ClaudeCheckpointModel {
    reject_guidance: bool,
    canonical: Value,
    fallback: String,
    requests: Arc<Mutex<Vec<(bool, Value)>>>,
}

#[allow(refining_impl_trait)]
impl CompletionModel for ClaudeCheckpointModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        unreachable!("the fixture constructs its model explicitly")
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        unreachable!("the owned summarizer uses streaming")
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let guided = request.output_schema.is_some();
        let body = build_messages_body("claude-sonnet-5", &request).unwrap();
        let output = if guided {
            if !self.reject_guidance && body["output_config"]["format"]["type"] == "json_schema" {
                self.canonical.to_string()
            } else {
                "# Continuation checkpoint\nThe structured schema was unavailable.".into()
            }
        } else {
            let schema = schemars::schema_for!(summary::ContinuationCheckpoint)
                .as_value()
                .to_string();
            let texts = |value: &Value| {
                let mut texts = Vec::new();
                collect_texts(value, &mut texts);
                texts
            };
            assert!(
                texts(body["messages"].as_array().unwrap().last().unwrap())
                    .iter()
                    .any(|text| text.contains(&schema)),
                "the strict JSON instruction rides in the final user message"
            );
            assert!(
                !texts(&body["system"])
                    .iter()
                    .any(|text| text.contains(&schema)),
                "the system prefix stays the guided attempts' prefix"
            );
            self.fallback.clone()
        };
        self.requests.lock().unwrap().push((guided, body));
        let _ = crate::rendered_request::scope::claim_pending();
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter([
                Ok(RawStreamingChoice::Message(output)),
                Ok(RawStreamingChoice::FinalResponse(())),
            ]),
        )))
    }
}

/// Where `cache_control` breakpoints sit: "system" or "messages[i]".
fn cache_control_paths(body: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let marked = |blocks: &Value| {
        blocks.as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("cache_control").is_some())
        })
    };
    if marked(&body["system"]) {
        paths.push("system".to_string());
    }
    for (index, message) in body["messages"].as_array().unwrap().iter().enumerate() {
        if marked(&message["content"]) {
            paths.push(format!("messages[{index}]"));
        }
    }
    paths
}

fn collect_texts(value: &Value, texts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("text") {
                texts.push(text.clone());
            }
            if let Some(Value::String(text)) = map.get("content") {
                texts.push(text.clone());
            }
            map.values().for_each(|value| collect_texts(value, texts));
        }
        Value::Array(values) => values.iter().for_each(|value| collect_texts(value, texts)),
        _ => {}
    }
}

async fn exercise(reject_guidance: bool) {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let case = contract["checkpoint_fallback_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "scalar_current_work")
        .unwrap();
    for kind in [
        BackendProviderKind::ClaudeCliSubscription,
        BackendProviderKind::AnthropicApiKey,
    ] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = ClaudeCheckpointModel {
            reject_guidance,
            canonical: case["expected"].clone(),
            fallback: case["raw"].as_str().unwrap().into(),
            requests: requests.clone(),
        };
        let mut config = crate::end_to_end_test::test_loop_config();
        config.provider_input_counter = Arc::new(ProviderInputCounter::new(
            kind,
            OpenAiWireApi::ChatCompletions,
            "claude-sonnet-5",
        ));
        config.additional_params = Some(json!({
            (ADVERTISED_REASONING_EFFORTS_PARAM): ["high"], "output_config":{"effort":"high"},
        }));
        let engine = ProviderReductionEngine::new(Arc::new(model), config);
        let history = (0..12)
            .flat_map(|turn| {
                [
                    Message::user(format!("request {turn}: {}", "x".repeat(800))),
                    Message::assistant(format!("response {turn}: {}", "y".repeat(400))),
                ]
            })
            .collect();
        let result = engine
            .reduce(
                history,
                20_000,
                &ReductionOptions {
                    keep_recent_tokens: 50,
                    summary_max_output_tokens: 512,
                    mode: ReductionMode::Summarize,
                    ..Default::default()
                },
                ReductionAdmission::required(1, 0),
            )
            .await
            .unwrap();
        let reduced = result.exact_reduction().unwrap();
        assert!(reduced
            .checkpoint
            .contains(case["expected"]["current_work"][0].as_str().unwrap()));
        assert!(reduced
            .checkpoint
            .contains(case["expected"]["next_actions"][0].as_str().unwrap()));
        let requests = requests.lock().unwrap();
        assert!(!requests.is_empty());
        assert_eq!(
            requests.iter().filter(|(guided, _)| !guided).count(),
            usize::from(reject_guidance)
        );
        if !reject_guidance {
            assert_eq!(
                requests.len(),
                1,
                "schema delivery must avoid unnecessary fallback calls"
            );
        }
        if reject_guidance {
            assert_eq!(
                requests.iter().filter(|(guided, _)| *guided).count(),
                1 + MAX_CLAUDE_GUIDED_SUMMARY_RETRIES as usize,
                "Claude gets one guided retry before the fallback"
            );
            let body = |guided: bool| {
                requests
                    .iter()
                    .find(|(is_guided, _)| *is_guided == guided)
                    .map(|(_, body)| body.clone())
                    .unwrap()
            };
            let (guided, fallback) = (body(true), body(false));
            assert_eq!(
                fallback["system"], guided["system"],
                "the fallback's system blocks are byte-identical to the guided attempt's"
            );
            assert_eq!(fallback["tools"], guided["tools"]);
            let messages = |body: &Value| body["messages"].as_array().unwrap().clone();
            let (guided_messages, fallback_messages) = (messages(&guided), messages(&fallback));
            assert_eq!(guided_messages.len(), fallback_messages.len());
            let prefix = guided_messages.len() - 1;
            assert_eq!(
                fallback_messages[..prefix],
                guided_messages[..prefix],
                "every message before the final instruction, cache_control included, is identical"
            );
            assert_ne!(fallback_messages[prefix], guided_messages[prefix]);
            for body in [&guided, &fallback] {
                let marked = cache_control_paths(body);
                assert!(
                    marked.len() <= 4,
                    "Anthropic allows four breakpoints: {marked:?}"
                );
                assert!(
                    marked.contains(&format!("messages[{}]", prefix - 1)),
                    "a breakpoint closes the shared chunk history: {marked:?}"
                );
                assert!(
                    marked.contains(&format!("messages[{prefix}]")),
                    "{marked:?}"
                );
                assert!(marked.contains(&"system".to_string()), "{marked:?}");
            }
        }
        for (guided, body) in requests.iter() {
            assert_eq!(body["output_config"]["effort"], "high");
            assert_eq!(body["output_config"].get("format").is_some(), *guided);
            assert!(body.get("tools").is_none());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn checkpoint_wire_compaction_uses_the_claude_schema_without_fallback() {
    exercise(false).await;
}

#[tokio::test(start_paused = true)]
async fn checkpoint_wire_compaction_recovers_scalar_lists_in_the_single_fallback() {
    exercise(true).await;
}
