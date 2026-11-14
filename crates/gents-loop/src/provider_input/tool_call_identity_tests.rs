use super::*;
use gents_protocol::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
};
use serde_json::json;

pub(crate) type Identity = (String, Option<String>);

#[derive(Debug, PartialEq)]
pub(crate) struct Observed {
    pub id: String,
    pub call_id: Option<String>,
    pub openai_item_id: Option<String>,
}

pub(crate) fn messages(identities: &[Identity]) -> Vec<Message> {
    let mut messages = vec![Message::user("Earlier request")];
    for (index, (id, call_id)) in identities.iter().enumerate() {
        messages.push(Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: id.clone(),
                call_id: call_id.clone(),
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
                id: id.clone(),
                call_id: call_id.clone(),
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

pub(crate) fn body_identities(body: &Value) -> Vec<(Value, Value)> {
    let input = body["input"].as_array().unwrap();
    let calls = input
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    let results = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), results.len());
    calls
        .iter()
        .zip(results)
        .enumerate()
        .map(|(index, (call, result))| {
            assert_eq!(result["call_id"], call["call_id"]);
            assert_eq!(call["name"], "inspect");
            let args = match &call["arguments"] {
                Value::String(text) => serde_json::from_str::<Value>(text).unwrap(),
                value => value.clone(),
            };
            assert_eq!(args, json!({"path": format!("file-{index}")}));
            assert_eq!(result["output"], format!("result-{index}"));
            (call["id"].clone(), call["call_id"].clone())
        })
        .collect()
}

pub(crate) fn observed_identities(openai: &Value, codex: &Value, xai: &Value) -> Vec<Observed> {
    let openai = body_identities(openai);
    assert_eq!(body_identities(codex), openai);
    let xai = body_identities(xai);
    assert_eq!(openai.len(), xai.len());
    openai
        .into_iter()
        .zip(xai)
        .map(|((item_id, openai_call_id), (id, call_id))| {
            assert_eq!(openai_call_id, call_id);
            Observed {
                id: serde_json::from_value(id).unwrap(),
                call_id: serde_json::from_value(call_id).unwrap(),
                openai_item_id: serde_json::from_value(item_id).unwrap(),
            }
        })
        .collect()
}

#[test]
fn production_responses_projection_follows_the_lean_tool_identity_model() {
    use gents_lean_vocab::families::provider_tool_identity_projected::{
        HistoryInput, IdentityWire,
    };
    gents_lean_vocab::families::provider_tool_identity_projected::replay(|case: &HistoryInput| {
        let identities = case
            .input
            .iter()
            .map(|wire| (wire.id.clone(), wire.call_id.clone()))
            .collect::<Vec<_>>();
        let original = messages(&identities);
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
        let bodies = [
            BackendProviderKind::OpenAiCompatible,
            BackendProviderKind::ChatGptCodex,
            BackendProviderKind::XaiGrokOAuth,
        ]
        .map(|provider| {
            let counter = counter(provider);
            let body = counter.project_body(&request).unwrap();
            assert_eq!(counter.project_body(&request).unwrap(), body);
            assert!(counter.estimate_request(&request).unwrap() > 0);
            assert_eq!(serde_json::to_value(&request.chat_history).unwrap(), stored);
            assert_eq!(original, messages(&identities));
            body
        });
        observed_identities(&bodies[0], &bodies[1], &bodies[2])
            .into_iter()
            .map(|observed| IdentityWire {
                id: observed.id,
                call_id: observed.call_id,
                openai_item_id: observed.openai_item_id,
            })
            .collect()
    });
}

fn normalized_identities(messages: &[rig::message::Message]) -> Vec<Identity> {
    let mut calls = Vec::new();
    let mut results = Vec::new();
    for message in messages {
        match message {
            rig::message::Message::Assistant { content, .. } => {
                for part in content.iter() {
                    if let rig::message::AssistantContent::ToolCall(call) = part {
                        calls.push((call.id.clone(), call.call_id.clone()));
                    }
                }
            }
            rig::message::Message::User { content } => {
                for part in content.iter() {
                    if let rig::message::UserContent::ToolResult(result) = part {
                        results.push((result.id.clone(), result.call_id.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(calls, results);
    calls
}

#[test]
fn production_tool_identity_normalization_follows_the_lean_tool_identity_model() {
    use gents_lean_vocab::families::provider_tool_identity_normalized::{
        HistoryInput, IdentityWire,
    };
    gents_lean_vocab::families::provider_tool_identity_normalized::replay(|case: &HistoryInput| {
        let identities = case
            .input
            .iter()
            .map(|wire| (wire.id.clone(), wire.call_id.clone()))
            .collect::<Vec<_>>();
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
        let observed = profiles
            .iter()
            .map(|profile| {
                let mut projected = crate::rig_compat::to_rig_messages(&messages(&identities));
                for message in &mut projected {
                    normalize_tool_call_ids(*profile, message);
                }
                let normalized = normalized_identities(&projected);
                let once = serde_json::to_value(&projected).unwrap();
                assert_eq!(
                    once,
                    serde_json::to_value(crate::rig_compat::to_rig_messages(&messages(
                        &normalized
                    )))
                    .unwrap(),
                    "{profile:?}"
                );
                for message in &mut projected {
                    normalize_tool_call_ids(*profile, message);
                }
                assert_eq!(serde_json::to_value(&projected).unwrap(), once);
                normalized
            })
            .collect::<Vec<_>>();
        assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
        observed[0]
            .iter()
            .map(|(id, call_id)| IdentityWire {
                id: id.clone(),
                call_id: call_id.clone(),
                openai_item_id: crate::responses_normalize::openai_function_call_item_id(id)
                    .map(str::to_string),
            })
            .collect()
    });
}
