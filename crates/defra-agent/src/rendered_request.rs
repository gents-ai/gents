use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use rig::completion::CompletionRequest;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backend_provider::BackendProviderKind;
use crate::graphql::escape_graphql_string;

pub(crate) type RenderedRequestSink = Arc<
    dyn Fn(usize, CompletionRequest) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderedRequestSource {
    OpenAiResponses,
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

    fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChatCompletions => "openai_chat_completions",
        }
    }
}

#[derive(Debug, Clone)]
struct RequestIdentity {
    request_id: String,
    agent_did: String,
    behavior_id: String,
    session_id: String,
}

#[derive(Debug, Clone)]
struct RenderedRequestRow {
    rendered_request_key: String,
    request_id: String,
    turn_index: usize,
    agent_did: String,
    behavior_id: String,
    session_id: String,
    source: &'static str,
    request_json: String,
    messages_json: String,
    tools_json: String,
    tool_choice_json: String,
    sampling_json: String,
    prompt_hash: String,
    tools_hash: String,
}

pub(crate) fn persisted_rendered_request_sink(
    node: Arc<EmbeddedNode>,
    request: &crate::watcher::AgentRequest,
    model_name: String,
    source: RenderedRequestSource,
) -> RenderedRequestSink {
    let identity = RequestIdentity {
        request_id: request.request_id.clone(),
        agent_did: request.agent_did.clone(),
        behavior_id: request.behavior_id.clone().unwrap_or_default(),
        session_id: request.session_id.clone(),
    };
    Arc::new(move |turn_index, request| {
        let node = node.clone();
        let identity = identity.clone();
        let model_name = model_name.clone();
        Box::pin(async move {
            let row = rendered_request_row(identity, turn_index, &model_name, source, request)?;
            persist_rendered_request(node.as_ref(), &row).await
        })
    })
}

fn rendered_request_row(
    identity: RequestIdentity,
    turn_index: usize,
    model_name: &str,
    source: RenderedRequestSource,
    request: CompletionRequest,
) -> Result<RenderedRequestRow> {
    let request_json = provider_request_json(model_name, source, request.clone())?;
    let messages = provider_messages(&request_json, source);
    let tools = request_json
        .get("tools")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let tool_choice = request_json
        .get("tool_choice")
        .cloned()
        .unwrap_or(Value::Null);
    let sampling = json!({
        "temperature": request.temperature,
        "max_tokens": request.max_tokens,
        "additional_params": request.additional_params.unwrap_or(Value::Null),
    });
    let prompt_hash = sha256_canonical_json(&messages)?;
    let tools_hash = sha256_canonical_json(&tools)?;

    Ok(RenderedRequestRow {
        rendered_request_key: format!("{}:{turn_index}", identity.request_id),
        request_id: identity.request_id,
        turn_index,
        agent_did: identity.agent_did,
        behavior_id: identity.behavior_id,
        session_id: identity.session_id,
        source: source.as_str(),
        request_json: serde_json::to_string(&request_json).context("encoding provider request")?,
        messages_json: serde_json::to_string(&messages).context("encoding provider messages")?,
        tools_json: serde_json::to_string(&tools).context("encoding provider tools")?,
        tool_choice_json: serde_json::to_string(&tool_choice)
            .context("encoding provider tool choice")?,
        sampling_json: serde_json::to_string(&sampling).context("encoding sampling")?,
        prompt_hash,
        tools_hash,
    })
}

fn provider_request_json(
    model_name: &str,
    source: RenderedRequestSource,
    request: CompletionRequest,
) -> Result<Value> {
    match source {
        RenderedRequestSource::OpenAiResponses => {
            let provider_request =
                rig::providers::openai::responses_api::CompletionRequest::try_from((
                    model_name.to_string(),
                    request,
                ))
                .context("rendering OpenAI Responses request")?;
            serde_json::to_value(provider_request).context("encoding OpenAI Responses request")
        }
        RenderedRequestSource::OpenAiChatCompletions => {
            let provider_request = rig::providers::openai::CompletionRequest::try_from((
                model_name.to_string(),
                request,
            ))
            .context("rendering OpenAI Chat Completions request")?;
            serde_json::to_value(provider_request)
                .context("encoding OpenAI Chat Completions request")
        }
    }
}

fn provider_messages(request_json: &Value, source: RenderedRequestSource) -> Value {
    match source {
        RenderedRequestSource::OpenAiResponses => request_json
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        RenderedRequestSource::OpenAiChatCompletions => request_json
            .get("messages")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    }
}

async fn persist_rendered_request(node: &EmbeddedNode, row: &RenderedRequestRow) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = rendered_request_upsert_mutation(row, &now);
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "persisting AgentRenderedRequest failed: {:?}",
            response.errors
        );
    }
    Ok(())
}

fn rendered_request_upsert_mutation(row: &RenderedRequestRow, now: &str) -> String {
    let rendered_request_key = escape_graphql_string(&row.rendered_request_key);
    let request_id = escape_graphql_string(&row.request_id);
    let agent_did = escape_graphql_string(&row.agent_did);
    let behavior_id = escape_graphql_string(&row.behavior_id);
    let session_id = escape_graphql_string(&row.session_id);
    let source = escape_graphql_string(row.source);
    let request_json = escape_graphql_string(&row.request_json);
    let messages_json = escape_graphql_string(&row.messages_json);
    let tools_json = escape_graphql_string(&row.tools_json);
    let tool_choice_json = escape_graphql_string(&row.tool_choice_json);
    let sampling_json = escape_graphql_string(&row.sampling_json);
    let prompt_hash = escape_graphql_string(&row.prompt_hash);
    let tools_hash = escape_graphql_string(&row.tools_hash);
    let now = escape_graphql_string(now);

    format!(
        r#"mutation {{
            upsert_AgentRenderedRequest(
                filter: {{ rendered_request_key: {{ _eq: "{rendered_request_key}" }} }},
                add: {{
                    rendered_request_key: "{rendered_request_key}",
                    request_id: "{request_id}",
                    turn_index: {turn_index},
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    source: "{source}",
                    request_json: "{request_json}",
                    messages_json: "{messages_json}",
                    tools_json: "{tools_json}",
                    tool_choice_json: "{tool_choice_json}",
                    sampling_json: "{sampling_json}",
                    prompt_hash: "{prompt_hash}",
                    tools_hash: "{tools_hash}",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    request_json: "{request_json}",
                    messages_json: "{messages_json}",
                    tools_json: "{tools_json}",
                    tool_choice_json: "{tool_choice_json}",
                    sampling_json: "{sampling_json}",
                    prompt_hash: "{prompt_hash}",
                    tools_hash: "{tools_hash}",
                    source: "{source}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        turn_index = row.turn_index,
    )
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
    use rig::completion::message::{Message, Text, ToolChoice, UserContent};
    use rig::completion::{CompletionRequest, ToolDefinition};
    use rig::one_or_many::OneOrMany;

    use super::*;

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![
                Message::system("You are exact."),
                Message::User {
                    content: OneOrMany::one(UserContent::Text(Text {
                        text: "Read the file.".to_string(),
                    })),
                },
            ])
            .expect("non-empty history"),
            documents: Vec::new(),
            tools: vec![ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            }],
            temperature: Some(0.2),
            max_tokens: Some(512),
            tool_choice: Some(ToolChoice::Auto),
            additional_params: Some(json!({
                "reasoning": { "effort": "medium" }
            })),
            output_schema: None,
        }
    }

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
    fn renders_openai_chat_provider_shape() {
        let row = rendered_request_row(
            RequestIdentity {
                request_id: "req-1".to_string(),
                agent_did: "did:key:test".to_string(),
                behavior_id: "behavior".to_string(),
                session_id: "session".to_string(),
            },
            0,
            "test-model",
            RenderedRequestSource::OpenAiChatCompletions,
            sample_request(),
        )
        .expect("render request");

        let messages: Value = serde_json::from_str(&row.messages_json).unwrap();
        let tools: Value = serde_json::from_str(&row.tools_json).unwrap();
        let sampling: Value = serde_json::from_str(&row.sampling_json).unwrap();

        assert_eq!(row.rendered_request_key, "req-1:0");
        assert_eq!(row.source, "openai_chat_completions");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(tools[0]["function"]["name"], "read_file");
        assert_eq!(sampling["temperature"], 0.2);
        assert_eq!(sampling["max_tokens"], 512);
        assert_eq!(row.prompt_hash.len(), 64);
        assert_eq!(row.tools_hash.len(), 64);
    }

    #[tokio::test]
    async fn persisted_sink_writes_rendered_request_row() {
        let data_path =
            std::env::temp_dir().join(format!("rendered-request-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_schemas(&node).await.unwrap();

        let request = crate::watcher::AgentRequest {
            doc_id: "doc-1".to_string(),
            request_id: "req-1".to_string(),
            agent_did: "did:key:test".to_string(),
            behavior_id: Some("behavior".to_string()),
            session_id: "session".to_string(),
            content: "Read the file.".to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            metadata: None,
            execution_origin: None,
            created_at: "2026-06-15T00:00:00Z".to_string(),
            deadline: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_tool_call_id: None,
        };
        let sink = persisted_rendered_request_sink(
            node.clone(),
            &request,
            "test-model".to_string(),
            RenderedRequestSource::OpenAiChatCompletions,
        );

        sink(0, sample_request()).await.unwrap();

        let response = node
            .execute(
                r#"{
                    AgentRenderedRequest(filter: { request_id: { _eq: "req-1" } }, limit: 1) {
                        rendered_request_key request_id turn_index agent_did behavior_id session_id
                        source messages_json tools_json sampling_json prompt_hash tools_hash
                    }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let row = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRenderedRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .expect("rendered request row");

        assert_eq!(row["rendered_request_key"], "req-1:0");
        assert_eq!(row["agent_did"], "did:key:test");
        assert_eq!(row["source"], "openai_chat_completions");
        assert!(row["messages_json"]
            .as_str()
            .unwrap()
            .contains("Read the file."));
        assert!(row["tools_json"].as_str().unwrap().contains("read_file"));
        assert_eq!(row["prompt_hash"].as_str().unwrap().len(), 64);
        assert_eq!(row["tools_hash"].as_str().unwrap().len(), 64);
    }
}
