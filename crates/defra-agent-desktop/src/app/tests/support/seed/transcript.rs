use super::*;

pub(crate) async fn insert_chat_transcript_documents(
    core: &ClientCore,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
    response_key: &str,
) -> Result<()> {
    let response_content = "Queue checked.\n\n- Found the audit target.\n- Ready to continue.";
    let response_reasoning =
        "I verified the latest request, ran the shell tool, and summarized the result.";
    let assistant_tool_call_message = serde_json::to_string(&Message::Assistant {
        id: None,
        content: OneOrMany::many(vec![
            AssistantContent::ToolCall(ToolCall {
                id: "call-shell-1".to_string(),
                call_id: Some("call-shell-1".to_string()),
                function: ToolFunction {
                    name: "shell".to_string(),
                    arguments: serde_json::json!({ "cmd": "rg audit" }),
                },
                signature: None,
                additional_params: None,
            }),
            AssistantContent::Text(Text {
                text: "I checked the queue and opened the trace.".to_string(),
            }),
        ])
        .context("assistant tool-call content")?,
    })?;
    let tool_result_message = serde_json::to_string(&Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: "call-shell-1".to_string(),
            call_id: Some("call-shell-1".to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: "src/app.rs: audit target live".to_string(),
            })),
        })),
    })?;
    let assistant_final_message = serde_json::to_string(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text {
            text: "Queue checked.\n\n- Found the audit target.\n- Ready to continue.".to_string(),
        })),
    })?;

    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            add_AgentMessage(input: {{
                message_key: "msg-assistant-1"
                session_id: "{session_id}"
                sequence: 2
                role: "assistant"
                content: "{assistant_tool_call_message}"
                timestamp: "2026-04-14T00:00:01Z"
            }}) {{ message_key }}
            add_AgentMessage(input: {{
                message_key: "msg-tool-result-1"
                session_id: "{session_id}"
                sequence: 3
                role: "user"
                content: "{tool_result_message}"
                timestamp: "2026-04-14T00:00:03Z"
            }}) {{ message_key }}
            add_AgentMessage(input: {{
                message_key: "msg-assistant-2"
                session_id: "{session_id}"
                sequence: 4
                role: "assistant"
                content: "{assistant_final_message}"
                timestamp: "2026-04-14T00:00:04Z"
            }}) {{ message_key }}
            add_AgentToolCall(input: {{
                tool_call_key: "tool-call-1"
                session_id: "{session_id}"
                message_sequence: 2
                tool_name: "shell"
                tool_call_id: "call-shell-1"
                args: "{{\"cmd\":\"rg audit\"}}"
                status: "completed"
                started_at: "2026-04-14T00:00:02Z"
                completed_at: "2026-04-14T00:00:03Z"
            }}) {{ tool_call_key }}
            add_AgentToolResult(input: {{
                agent_did: "{agent_did}"
                session_id: "{session_id}"
                tool_name: "shell"
                tool_input: "rg audit"
                output_text: "src/app.rs: audit target live"
                truncated: false
                truncation_metadata: ""
                conversation_doc_id: "{session_id}"
                created_at: "2026-04-14T00:00:03Z"
            }}) {{ _docID }}
            add_AgentResponse(input: {{
                response_key: "{response_key}"
                agent_did: "{agent_did}"
                behavior_id: "{behavior_id}"
                session_id: "{session_id}"
                content: "{response_content}"
                reasoning: "{response_reasoning}"
                status: "completed"
                error_message: ""
                token_count: 42
                progress_seq: 1
                created_at: "2026-04-14T00:00:04Z"
                completed_at: "2026-04-14T00:00:05Z"
            }}) {{ response_key }}
        }}"#,
            session_id = escape_graphql_string(session_id),
            agent_did = escape_graphql_string(agent_did),
            behavior_id = escape_graphql_string(behavior_id),
            response_key = escape_graphql_string(response_key),
            assistant_tool_call_message = escape_graphql_string(&assistant_tool_call_message),
            tool_result_message = escape_graphql_string(&tool_result_message),
            assistant_final_message = escape_graphql_string(&assistant_final_message),
            response_content = escape_graphql_string(response_content),
            response_reasoning = escape_graphql_string(response_reasoning),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "insert chat transcript documents failed: {:?}",
            response.errors
        );
    }
    core.refresh_store().await?;
    Ok(())
}
