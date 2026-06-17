use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backend_provider::BackendProviderKind;

pub type RenderedRequestCaptureSink = Arc<
    dyn Fn(RenderedCompletionRequest) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

pub type RenderedRequestCaptureFactory =
    Arc<dyn Fn(RenderedRequestContext) -> RenderedRequestCaptureSink + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderedRequestSource {
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
}

impl RenderedRequestSource {
    pub(crate) fn for_behavior_provider(kind: BackendProviderKind) -> Self {
        match kind {
            BackendProviderKind::OpenAiCompatible => {
                if crate::inference_http::force_openai_chat_completions() {
                    Self::OpenAiChatCompletions
                } else {
                    Self::OpenAiResponses
                }
            }
            BackendProviderKind::OpenRouter => Self::OpenAiChatCompletions,
            BackendProviderKind::ChatGptCodex => Self::OpenAiResponses,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedRequestContext {
    pub request_id: String,
    pub agent_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub model_name: String,
    pub source: RenderedRequestSource,
}

impl RenderedRequestContext {
    pub(crate) fn for_request(
        request: &crate::watcher::AgentRequest,
        model_name: String,
        source: RenderedRequestSource,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            agent_did: request.agent_did.clone(),
            behavior_id: request.behavior_id.clone().unwrap_or_default(),
            session_id: request.session_id.clone(),
            model_name,
            source,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedCompletionRequest {
    pub request_id: String,
    pub turn_index: usize,
    pub agent_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub model_name: String,
    pub source: RenderedRequestSource,
    pub request_json: Value,
    pub messages_json: Value,
    pub tools_json: Value,
    pub tool_choice_json: Value,
    pub sampling_json: Value,
    pub prompt_hash: String,
    pub tools_hash: String,
}

pub(crate) fn build_rendered_completion_request(
    context: &RenderedRequestContext,
    turn_index: usize,
    request_json: Value,
    messages_json: Value,
    tools_json: Value,
    tool_choice_json: Value,
    sampling_json: Value,
) -> Result<RenderedCompletionRequest> {
    let prompt_hash = sha256_canonical_json(&messages_json)?;
    let tools_hash = sha256_canonical_json(&tools_json)?;

    Ok(RenderedCompletionRequest {
        request_id: context.request_id.clone(),
        turn_index,
        agent_did: context.agent_did.clone(),
        behavior_id: context.behavior_id.clone(),
        session_id: context.session_id.clone(),
        model_name: context.model_name.clone(),
        source: context.source,
        request_json,
        messages_json,
        tools_json,
        tool_choice_json,
        sampling_json,
        prompt_hash,
        tools_hash,
    })
}

pub(crate) fn sampling_json(
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    additional_params: Option<Value>,
) -> Value {
    json!({
        "temperature": temperature,
        "max_tokens": max_tokens,
        "additional_params": additional_params.unwrap_or(Value::Null),
    })
}

fn sha256_canonical_json(value: &Value) -> Result<String> {
    let canonical = canonical_json(value);
    let encoded = serde_json::to_vec(&canonical).context("encoding canonical JSON")?;
    let digest = Sha256::digest(encoded);
    Ok(format!("{digest:x}"))
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_hash_sorts_object_keys() {
        let left = json!({ "b": 1, "a": { "d": 2, "c": 3 } });
        let right = json!({ "a": { "c": 3, "d": 2 }, "b": 1 });

        assert_eq!(
            sha256_canonical_json(&left).unwrap(),
            sha256_canonical_json(&right).unwrap()
        );
    }

    #[test]
    fn rendered_completion_request_hashes_prompt_and_tools() {
        let context = RenderedRequestContext {
            request_id: "req-1".to_string(),
            agent_did: "did:key:test".to_string(),
            behavior_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "test-model".to_string(),
            source: RenderedRequestSource::OpenAiChatCompletions,
        };
        let rendered = build_rendered_completion_request(
            &context,
            0,
            json!({"messages": [{"role": "user", "content": "hi"}]}),
            json!([{"role": "user", "content": "hi"}]),
            json!([{"type": "function", "function": {"name": "read_file"}}]),
            Value::Null,
            sampling_json(
                Some(0.2),
                Some(512),
                Some(json!({"reasoning": {"effort": "medium"}})),
            ),
        )
        .expect("rendered request");

        assert_eq!(rendered.request_id, "req-1");
        assert_eq!(rendered.turn_index, 0);
        assert_eq!(
            rendered.source,
            RenderedRequestSource::OpenAiChatCompletions
        );
        assert_eq!(rendered.messages_json[0]["role"], "user");
        assert_eq!(rendered.tools_json[0]["function"]["name"], "read_file");
        assert_eq!(rendered.sampling_json["temperature"], 0.2);
        assert_eq!(rendered.sampling_json["max_tokens"], 512);
        assert_eq!(rendered.prompt_hash.len(), 64);
        assert_eq!(rendered.tools_hash.len(), 64);
    }
}
