use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SessionContextDetails {
    pub system_prompt_tokens: u64,
    pub message_tokens: u64,
    pub user_input_tokens: u64,
    pub assistant_tokens: u64,
    pub tool_call_tokens: u64,
    pub tool_result_tokens: u64,
    pub other_message_tokens: u64,
    pub message_count: u64,
    pub tool_definitions_count: u64,
    pub tool_call_count: u64,
}

pub async fn load_session_context_details(
    access: &crate::config_client::ConfigAccess,
    agent: &str,
    requester: Option<&str>,
    session: &str,
    context: &super::super::context_budget::LastRequestContextSnapshot,
) -> Result<Option<SessionContextDetails>> {
    anyhow::ensure!(
        !agent.trim().is_empty() && !session.trim().is_empty(),
        "context details require an exact session scope"
    );
    if context.accounting.estimator != "openai_chat_wire_json_bytes_div_4_v1"
        || context.accounting.components.documents != 0
    {
        return Ok(None);
    }
    let requester = requester
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{_eq: "{}"}}, requester_did: {{_eq: {requester}}},
        session_id: {{_eq: "{}"}}, request_id: {{_eq: "{}"}}
    }}, limit: 2) {{_docID}} }}"#,
            escape_graphql_string(agent),
            escape_graphql_string(session),
            escape_graphql_string(&context.request_id)
        ))
        .await?;
    let owners = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("missing context detail owners")?;
    let [owner] = owners.as_slice() else {
        return Ok(None);
    };
    let doc = owner["_docID"]
        .as_str()
        .context("context detail owner lacks physical identity")?;
    let response = access
        .execute(&format!(
            r#"{{ RenderedRequest(filter: {{
        request_doc_id: {{_eq: "{}"}}, node_did: {{_eq: "{}"}},
        session_id: {{_eq: "{}"}}, capture_scope: {{_like: "inference.%"}},
        turn_index: {{_eq: {}}}, attempt: {{_eq: {}}}
    }}, limit: 2) {{capture_version source request_json provenance_json}} }}"#,
            escape_graphql_string(doc),
            escape_graphql_string(agent),
            escape_graphql_string(session),
            context.accounting.turn_index,
            context.accounting.attempt
        ))
        .await?;
    let captures = response
        .pointer("/data/RenderedRequest")
        .and_then(Value::as_array)
        .context("missing context detail captures")?;
    let [capture] = captures.as_slice() else {
        return Ok(None);
    };
    let Some(capture_version) = capture["capture_version"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
    else {
        return Ok(None);
    };
    if !(1..=gents_protocol::rendered_request::CAPTURE_VERSION).contains(&capture_version) {
        return Ok(None);
    }
    let details: Result<Option<SessionContextDetails>> = async {
        let provenance: Value = serde_json::from_str(
            capture["provenance_json"]
                .as_str()
                .context("missing capture provenance")?,
        )?;
        if provenance["admission"]["call_id"].as_str() != Some(context.call_id.as_str()) {
            return Ok(None);
        }
        if capture["source"].as_str() != Some("openai_chat_completions") {
            return Ok(None);
        }
        let body = crate::rendered_request::decode_capture_json(
            access,
            capture_version,
            capture["request_json"]
                .as_str()
                .context("missing captured provider request")?,
            crate::rendered_request::CapturePayloadKind::RequestBody,
        )
        .await?;
        chat_details(&body, context.accounting.components.messages as u64).map(Some)
    }
    .await;
    match details {
        Ok(details) => Ok(details),
        Err(error) => {
            tracing::debug!(
                node_did = agent,
                session_id = session,
                request_id = %context.request_id,
                call_id = %context.call_id,
                %error,
                "optional session context details could not decode provider capture"
            );
            Ok(None)
        }
    }
}

fn chat_details(body: &Value, message_tokens: u64) -> Result<SessionContextDetails> {
    let messages = body["messages"]
        .as_array()
        .context("captured chat messages are not an array")?;
    anyhow::ensure!(
        crate::provider_input::estimate_json(&body["messages"])? as u64 == message_tokens,
        "captured message estimate differs from recorded accounting"
    );
    let mut category_bytes = [0_usize; 5];
    let mut message_count = 0_u64;
    let mut tool_call_count = 0_u64;
    for message in messages {
        let role = message["role"].as_str();
        if !matches!(role, Some("system" | "developer")) {
            message_count += 1;
        }
        if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
            tool_call_count += calls
                .as_array()
                .context("captured tool calls are not an array")?
                .len() as u64;
        }
        let bytes = serde_json::to_vec(message)?.len();
        match role {
            Some("system" | "developer") => category_bytes[0] += bytes,
            Some("user")
                if message_text(message)
                    .is_some_and(crate::background_completion::is_tool_completion_text) =>
            {
                category_bytes[4] += bytes
            }
            Some("user") if injected_context_message(message) => category_bytes[0] += bytes,
            Some("user") => category_bytes[1] += bytes,
            Some("assistant") => {
                let mut without_calls = message.clone();
                let fields = without_calls
                    .as_object_mut()
                    .context("captured assistant message is not an object")?;
                for field in ["tool_calls", "function_call"] {
                    if fields.get(field).is_some_and(|value| !value.is_null()) {
                        fields.remove(field);
                    }
                }
                let assistant_bytes = serde_json::to_vec(&without_calls)?.len();
                category_bytes[2] += assistant_bytes;
                category_bytes[3] += bytes - assistant_bytes;
            }
            Some("tool" | "function") => category_bytes[4] += bytes,
            _ => {}
        }
    }
    let [system_prompt_tokens, user_input_tokens, assistant_tokens, tool_call_tokens, tool_result_tokens] =
        category_bytes.map(|bytes| (bytes / 4) as u64);
    let message_tokens = message_tokens
        .checked_sub(system_prompt_tokens)
        .context("system estimate exceeds captured message accounting")?;
    // Unclassified fields, array framing and integer-rounding remainders stay
    // explicit so the partition equals the persisted provider-input estimate.
    let other_message_tokens = message_tokens
        .checked_sub(user_input_tokens + assistant_tokens + tool_call_tokens + tool_result_tokens)
        .context("message breakdown exceeds recorded accounting")?;
    let tool_definitions_count = match body.get("tools") {
        None | Some(Value::Null) => 0,
        Some(tools) => tools
            .as_array()
            .context("captured tools are not an array")?
            .len() as u64,
    };
    Ok(SessionContextDetails {
        system_prompt_tokens,
        message_tokens,
        user_input_tokens,
        assistant_tokens,
        tool_call_tokens,
        tool_result_tokens,
        other_message_tokens,
        message_count,
        tool_definitions_count,
        tool_call_count,
    })
}

fn message_text(message: &Value) -> Option<&str> {
    let content = &message["content"];
    content.as_str().or_else(|| {
        let [part] = content.as_array()?.as_slice() else {
            return None;
        };
        (part["type"].as_str() == Some("text"))
            .then(|| part["text"].as_str())
            .flatten()
    })
}

fn injected_context_message(message: &Value) -> bool {
    message_text(message).is_some_and(|text| {
        gents_loop::prompt::is_system_reminder(text)
            || gents_loop::loop_stream::is_request_context_message(
                &gents_protocol::message::Message::User {
                    content: vec![gents_protocol::message::UserContent::Text(
                        gents_protocol::message::Text {
                            text: text.to_owned(),
                        },
                    )],
                },
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_details_classify_background_completions_as_tool_results() {
        let (notification, _) = crate::background_completion::tool_completion_presentation(
            "call",
            "shell",
            "complete",
            "Compilation succeeded",
            None,
            4000,
        );
        let body = json!({"messages":[{"role":"user","content":notification}]});
        let total = crate::provider_input::estimate_json(&body["messages"]).unwrap() as u64;
        let details = chat_details(&body, total).unwrap();
        assert_eq!(details.user_input_tokens, 0);
        assert!(details.tool_result_tokens > 0);
        assert_eq!(
            details.tool_result_tokens + details.other_message_tokens,
            total
        );
    }

    #[test]
    fn checkpoint_skill_and_request_context_messages_are_not_counted_as_user_input() {
        use gents_protocol::message::{Message, UserContent};
        let Message::User { content } = gents_loop::prompt::compaction_summary_message(&[
            "Earlier conversation checkpoint".to_owned(),
        ])
        .unwrap() else {
            panic!("checkpoint message")
        };
        let UserContent::Text(checkpoint) = &content[0] else {
            panic!("checkpoint text")
        };
        let Message::User { content } =
            gents_loop::prompt::system_reminder("Activated skill instructions")
        else {
            panic!("skill message")
        };
        let UserContent::Text(skill) = &content[0] else {
            panic!("skill text")
        };
        let user = json!({"role":"user", "content":"Please summarize the changes."});
        let body = json!({"messages":[
            {"role":"user", "content":checkpoint.text},
            {"role":"user", "content":[{"type":"text","text":skill.text}]},
            {"role":"user", "content":"<context>Request instructions</context>"},
            user,
        ]});
        let total = crate::provider_input::estimate_json(&body["messages"]).unwrap() as u64;
        let details = chat_details(&body, total).unwrap();
        assert_eq!(
            details.user_input_tokens,
            crate::provider_input::estimate_json(&user).unwrap() as u64
        );
        assert!(details.system_prompt_tokens > details.user_input_tokens);
        assert_eq!(details.system_prompt_tokens + details.message_tokens, total);
        for content in [
            json!("A user quoting <system-reminder>something</system-reminder>"),
            json!([{"type":"text","text":skill.text}, {"type":"text","text":"Actual user input"}]),
        ] {
            assert!(!injected_context_message(
                &json!({"role":"user","content":content})
            ));
        }
    }

    #[test]
    fn chat_details_partition_messages_and_count_actual_provider_tools() {
        let body = json!({"messages":[
            {"role":"system","content":"system instruction"},
            {"role":"developer","content":"developer instruction"},
            {"role":"user","content":"hello"},
            {"role":"assistant","tool_calls":[{"id":"a"},{"id":"b"}]},
            {"role":"tool","content":"done"}
        ],"tools":[{"type":"function"}]});
        let total = crate::provider_input::estimate_json(&body["messages"]).unwrap() as u64;
        let details = chat_details(&body, total).unwrap();
        assert!(details.system_prompt_tokens > 0);
        assert_eq!(details.system_prompt_tokens + details.message_tokens, total);
        assert_eq!(details.message_count, 3);
        assert_eq!(details.tool_call_count, 2);
        assert_eq!(details.tool_definitions_count, 1);
        assert!(details.user_input_tokens > 0);
        assert!(details.assistant_tokens > 0);
        assert!(details.tool_call_tokens > 0);
        assert!(details.tool_result_tokens > 0);
        assert_eq!(
            details.user_input_tokens
                + details.assistant_tokens
                + details.tool_call_tokens
                + details.tool_result_tokens
                + details.other_message_tokens,
            details.message_tokens
        );
        assert!(chat_details(&body, 0).is_err());
        assert!(chat_details(&json!({"messages":[],"tools":{}}), 0).is_err());
    }

    #[test]
    fn breakdown_preserves_serialized_byte_accounting_without_double_counting_calls() {
        for body in [
            json!({"messages": []}),
            json!({"messages": [{"role": "user", "content": "🦀 café"}]}),
            json!({"messages": [
                {"role": "system", "content": "instructions"},
                {"role": "developer", "content": "context"},
                {"role": "user", "content": [{"type": "text", "text": "question"}]},
                {"role": "assistant", "content": "answer", "tool_calls": [
                    {"id": "call-1", "function": {"name": "query", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call-1", "content": "result"},
                {"role": "extension", "content": "additional message data"}
            ]}),
        ] {
            let total = crate::provider_input::estimate_json(&body["messages"]).unwrap() as u64;
            let details = chat_details(&body, total).unwrap();
            assert_eq!(
                details.system_prompt_tokens
                    + details.user_input_tokens
                    + details.assistant_tokens
                    + details.tool_call_tokens
                    + details.tool_result_tokens
                    + details.other_message_tokens,
                total
            );
            assert!(chat_details(&body, total + 1).is_err());
        }
    }
}
