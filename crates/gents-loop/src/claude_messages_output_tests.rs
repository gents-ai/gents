use super::*;

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
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["claude_output_config_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 10);
    for case in cases {
        let mut request = request();
        request.output_schema = if case["schema"].is_null() {
            None
        } else {
            Some(serde_json::from_value(case["schema"].clone()).unwrap())
        };
        request.additional_params = Some(json!({
            (ADVERTISED_REASONING_EFFORTS_PARAM): case["supported"],
            "output_config": {"effort":case["effort"], "format":{"type":"text"}, "untrusted":true},
            "temperature":0.5,
        }));
        let body = build_messages_body("claude-sonnet-5", &request).unwrap();
        assert_eq!(body["output_config"], case["expected"], "{case}");
        assert!(body.get("temperature").is_none());
        assert!(body["output_config"].get("untrusted").is_none());
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
            let expected_schema_tokens = case["expected"]
                .get("format")
                .map(crate::provider_input::estimate_json)
                .transpose()
                .unwrap()
                .unwrap_or(0);
            assert_eq!(
                projection.components.output_schema, expected_schema_tokens,
                "{case}"
            );
            assert_eq!(
                projection.estimated_input_tokens,
                counter.estimate_request(&request).unwrap()
            );
        }
    }
}
