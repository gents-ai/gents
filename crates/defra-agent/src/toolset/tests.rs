use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;

use super::args::{EditFileArgs, ReadFileArgs, WriteFileArgs};
use super::delegate::DelegateToAgentArgs;
use super::file_tools::{EditFileTool, ReadFileTool, WriteFileTool};
use super::shared::{validate_read_only_command, ToolContext};
use super::*;
use crate::ensure_schemas;
use crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES;

#[test]
fn toolset_presets_have_expected_counts() {
    assert_eq!(ToolSet::readonly().native_tools().len(), 5);
    assert_eq!(
        ToolSet::readwrite(std::env::temp_dir())
            .native_tools()
            .len(),
        8
    );
    assert_eq!(ToolSet::meta_only().native_tools().len(), 0);
}

fn temp_root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[tokio::test]
async fn read_file_returns_numbered_contents() {
    let root = temp_root("defra-agent-read-file");
    let file = root.join("notes.txt");
    std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();
    let tool = ReadFileTool::new(
        ToolContext::new(root, false).unwrap(),
        DEFAULT_MAX_FILE_CHARS,
    );

    let output = rig::tool::Tool::call(
        &tool,
        ReadFileArgs {
            path: "notes.txt".to_string(),
            start_line: Some(2),
            end_line: Some(3),
            max_chars: DEFAULT_MAX_FILE_CHARS,
        },
    )
    .await
    .unwrap();

    assert!(output.contains("2: beta"));
    assert!(output.contains("3: gamma"));
}

#[tokio::test]
async fn write_and_edit_file_work_under_root() {
    let root = temp_root("defra-agent-write-edit");
    let context = ToolContext::new(root.clone(), true).unwrap();
    let writer = WriteFileTool::new(context.clone());
    let editor = EditFileTool::new(context);

    rig::tool::Tool::call(
        &writer,
        WriteFileArgs {
            path: "nested/file.txt".to_string(),
            content: "hello world".to_string(),
        },
    )
    .await
    .unwrap();
    rig::tool::Tool::call(
        &editor,
        EditFileArgs {
            path: "nested/file.txt".to_string(),
            old_text: "world".to_string(),
            new_text: "amy".to_string(),
            replace_all: false,
        },
    )
    .await
    .unwrap();

    let content = std::fs::read_to_string(root.join("nested/file.txt")).unwrap();
    assert_eq!(content, "hello amy");
}

#[test]
fn read_only_bash_rejects_write_commands() {
    assert!(validate_read_only_command(
        "git",
        &[String::from("commit")],
        &default_read_only_commands()
    )
    .is_err());
}

#[tokio::test]
async fn delegate_to_agent_round_trip_waits_for_response() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_schemas(node.as_ref()).await.unwrap();
    let tool = super::delegate::DelegateToAgentTool::new(
        node.clone(),
        vec!["did:defra-agent:amy-code".to_string()],
    );

    let call = tokio::spawn(async move {
        rig::tool::Tool::call(
            &tool,
            DelegateToAgentArgs {
                target_did: "did:defra-agent:amy-code".to_string(),
                content: "Write a test".to_string(),
                wait: true,
            },
        )
        .await
        .unwrap()
    });

    #[derive(serde::Deserialize)]
    struct RequestRow {
        request_id: String,
        agent_did: String,
        session_id: String,
        content: String,
        retry_count: i64,
        max_retries: i64,
    }

    let request = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let resp = node
                .execute(
                    r#"{
                            AgentRequest(limit: 1) {
                                request_id
                                agent_did
                                session_id
                                content
                                retry_count
                                max_retries
                            }
                        }"#,
                )
                .await;
            if !resp.has_errors() {
                let rows: Vec<RequestRow> = resp
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .unwrap_or_default();
                if let Some(row) = rows.into_iter().next() {
                    break row;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(request.agent_did, "did:defra-agent:amy-code");
    assert_eq!(request.content, "Write a test");
    assert_eq!(request.retry_count, 0);
    assert_eq!(request.max_retries, DEFAULT_REQUEST_MAX_RETRIES as i64);

    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
                create_AgentResponse(
                    input: {{
                        response_key: "{request_id}",
                        request_id: "{request_id}",
                        agent_did: "{agent_did}",
                        session_id: "{session_id}",
                        content: "delegated result",
                        status: "complete",
                        token_count: 2,
                        progress_seq: 1,
                        created_at: "{created_at}",
                        completed_at: "{created_at}"
                    }}
                ) {{ _docID }}
            }}"#,
        request_id = crate::graphql::escape_graphql_string(&request.request_id),
        agent_did = crate::graphql::escape_graphql_string(&request.agent_did),
        session_id = crate::graphql::escape_graphql_string(&request.session_id),
        created_at = crate::graphql::escape_graphql_string(&now),
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);

    let result = call.await.unwrap();
    assert_eq!(result, "delegated result");
}

#[tokio::test]
async fn delegate_to_agent_rejects_target_outside_allowlist() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_schemas(node.as_ref()).await.unwrap();
    let tool = super::delegate::DelegateToAgentTool::new(
        node.clone(),
        vec!["did:defra-agent:allowed".to_string()],
    );

    let error = rig::tool::Tool::call(
        &tool,
        DelegateToAgentArgs {
            target_did: "did:defra-agent:blocked".to_string(),
            content: "Write a test".to_string(),
            wait: false,
        },
    )
    .await
    .expect_err("delegate_to_agent should reject blocked targets");
    assert!(error
        .to_string()
        .contains("is not in the allowed delegation set"));

    let resp = node
        .execute(r#"{ AgentRequest(limit: 1) { request_id } }"#)
        .await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(rows.is_empty(), "blocked delegation wrote an AgentRequest");
}
