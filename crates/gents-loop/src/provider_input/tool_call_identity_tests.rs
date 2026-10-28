use super::*;
use gents_protocol::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
};
use rig::one_or_many::OneOrMany;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct Identity {
    pub id: String,
    pub call_id: Option<String>,
    pub openai_item_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Case {
    pub name: String,
    pub responses: bool,
    pub input: Vec<Identity>,
    pub expected: Vec<Identity>,
}

pub(crate) fn cases() -> Vec<Case> {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["provider_tool_identity_cases"].clone()).unwrap();
    assert!(!cases.is_empty());
    cases
}

pub(crate) fn messages(case: &Case) -> Vec<Message> {
    let mut messages = vec![Message::user("Earlier request")];
    for (index, identity) in case.input.iter().enumerate() {
        messages.push(Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: identity.id.clone(),
                call_id: identity.call_id.clone(),
                function: ToolFunction {
                    name: "inspect".into(),
                    arguments: json!({"path": format!("file-{index}")}),
                },
                signature: None,
                additional_params: None,
            })],
        });
        messages.push(Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: identity.id.clone(),
                call_id: identity.call_id.clone(),
                content: vec![ToolResultContent::text(format!("result-{index}"))],
            })],
        });
    }
    messages.push(Message::user("Continue this session"));
    messages
}

pub(crate) fn counter(provider: BackendProviderKind) -> ProviderInputCounter {
    ProviderInputCounter::new(provider, OpenAiWireApi::Responses, "test-model")
}

pub(crate) fn assert_responses_body(body: &Value, case: &Case, openai: bool) {
    let input = body["input"].as_array().unwrap();
    let calls = input
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    let results = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), case.expected.len(), "{}", case.name);
    assert_eq!(results.len(), case.expected.len(), "{}", case.name);
    for (index, ((call, result), expected)) in
        calls.iter().zip(results).zip(&case.expected).enumerate()
    {
        assert_eq!(
            call["id"],
            if openai {
                json!(expected.openai_item_id)
            } else {
                json!(expected.id)
            },
            "{}",
            case.name,
        );
        assert_eq!(call["call_id"], json!(expected.call_id), "{}", case.name);
        assert_eq!(result["call_id"], call["call_id"], "{}", case.name);
        assert_eq!(call["name"], "inspect");
        let args = match &call["arguments"] {
            Value::String(text) => serde_json::from_str::<Value>(text).unwrap(),
            value => value.clone(),
        };
        assert_eq!(args, json!({"path": format!("file-{index}")}));
        assert_eq!(result["output"], format!("result-{index}"));
    }
}

#[test]
fn modeled_cross_provider_tool_identity_projects_for_responses() {
    for case in cases().into_iter().filter(|case| case.responses) {
        let original = messages(&case);
        let request = CompletionRequest {
            model: None,
            preamble: Some("System instructions".into()),
            chat_history: OneOrMany::many(crate::rig_compat::to_rig_messages(&original)).unwrap(),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };
        let stored = serde_json::to_value(&request.chat_history).unwrap();
        for provider in [
            BackendProviderKind::OpenAiCompatible,
            BackendProviderKind::ChatGptCodex,
            BackendProviderKind::XaiGrokOAuth,
        ] {
            let counter = counter(provider);
            let body = counter.project_body(&request).unwrap();
            assert_responses_body(&body, &case, provider != BackendProviderKind::XaiGrokOAuth);
            assert_eq!(counter.project_body(&request).unwrap(), body);
            assert!(counter.estimate_request(&request).unwrap() > 0);
            assert_eq!(serde_json::to_value(&request.chat_history).unwrap(), stored);
            assert_eq!(original, messages(&case));
        }
    }
}

#[test]
fn modeled_tool_identity_projection_is_idempotent_and_preserves_other_fields() {
    for case in cases() {
        let profiles: &[ProviderInputProfile] = if case.responses {
            &[
                ProviderInputProfile::OpenAiResponsesNormalized,
                ProviderInputProfile::ChatGptCodexResponses,
                ProviderInputProfile::XaiResponses,
            ]
        } else {
            &[
                ProviderInputProfile::ClaudeMessages,
                ProviderInputProfile::OpenAiChatCompletions,
                ProviderInputProfile::OpenRouterChatCompletions,
            ]
        };
        for profile in profiles {
            let mut projected = crate::rig_compat::to_rig_messages(&messages(&case));
            let mut expected = messages(&case);
            let mut index = 0;
            for message in &mut expected {
                match message {
                    Message::Assistant { content, .. } => {
                        for part in content {
                            if let AssistantContent::ToolCall(call) = part {
                                assert_eq!(call.id, case.expected[index].id);
                                call.call_id = case.expected[index].call_id.clone();
                            }
                        }
                    }
                    Message::User { content } => {
                        for part in content {
                            if let UserContent::ToolResult(result) = part {
                                assert_eq!(result.id, case.expected[index].id);
                                result.call_id = case.expected[index].call_id.clone();
                                index += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            for message in &mut projected {
                normalize_tool_call_ids(*profile, message);
            }
            let once = serde_json::to_value(&projected).unwrap();
            assert_eq!(
                once,
                serde_json::to_value(crate::rig_compat::to_rig_messages(&expected)).unwrap(),
                "{} {profile:?}",
                case.name,
            );
            for message in &mut projected {
                normalize_tool_call_ids(*profile, message);
            }
            assert_eq!(serde_json::to_value(&projected).unwrap(), once);
        }
    }
}
