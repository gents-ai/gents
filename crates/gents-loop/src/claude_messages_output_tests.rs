use gents_protocol::message::Message;
use rig::completion::CompletionRequest;
use serde_json::json;

use super::*;
use gents_lean_vocab::families::compaction_checkpoint_claude_output_config as family;

fn request() -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: None,
        chat_history: rig::OneOrMany::one(crate::rig_compat::to_rig_message(&Message::user(
            "Continue the user's task",
        ))),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(512),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

#[test]
fn checkpoint_wire_claude_output_matches_executed_model() {
    family::replay(|i| {
        let mut request = request();
        request.output_schema = i
            .schema
            .as_deref()
            .map(|schema| serde_json::from_str(schema).unwrap());
        request.additional_params = Some(json!({
            (ADVERTISED_REASONING_EFFORTS_PARAM): i.supported,
            "output_config": {"effort": i.effort, "format":{"type":"text"}, "untrusted":true},
            "temperature":0.5,
        }));
        let body = build_messages_body("claude-sonnet-5", &request).unwrap();
        assert!(body.get("temperature").is_none());
        assert!(body["output_config"].get("untrusted").is_none());
        assert!(body["output_config"]
            .as_object()
            .is_none_or(|config| config.keys().all(|key| key == "effort" || key == "format")));
        for kind in [
            crate::backend_provider::BackendProviderKind::ClaudeCliSubscription,
            crate::backend_provider::BackendProviderKind::AnthropicApiKey,
        ] {
            let counter = crate::provider_input::ProviderInputCounter::new(
                kind,
                crate::openai_wire::OpenAiWireApi::ChatCompletions,
                "claude-sonnet-5",
            );
            assert_eq!(counter.project_body(&request).unwrap(), body);
            let projection = counter.project_request(&request).unwrap();
            let expected_schema_tokens = body["output_config"]
                .get("format")
                .map(crate::provider_input::estimate_json)
                .transpose()
                .unwrap()
                .unwrap_or(0);
            assert_eq!(
                projection.components.output_schema, expected_schema_tokens,
                "{body}"
            );
            assert_eq!(
                projection.estimated_input_tokens,
                counter.estimate_request(&request).unwrap()
            );
        }
        body.get("output_config")
            .map(|config| family::OutputConfig {
                effort: config["effort"].as_str().map(str::to_owned),
                format: config.get("format").map(|format| family::OutputFormat {
                    kind: format["type"].as_str().unwrap().to_owned(),
                    schema: serde_json::to_string(&format["schema"]).unwrap(),
                }),
            })
    });
}
