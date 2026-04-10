use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;

use super::args::{EditFileArgs, GlobArgs, GrepArgs, ListFilesArgs, ReadFileArgs, WriteFileArgs};
use super::delegate::DelegateToAgentArgs;
use super::file_tools::{
    EditFileTool, GlobTool, GrepTool, ListFilesTool, ReadFileTool, WriteFileTool,
};
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

    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["path"], "notes.txt");
    assert_eq!(value["start_line"], 2);
    assert_eq!(value["end_line"], 3);
    assert_eq!(value["returned_lines"], 2);
    assert_eq!(value["content"], "2: beta\n3: gamma");
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

#[cfg(unix)]
#[tokio::test]
async fn list_files_skips_permission_denied_subtrees() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("defra-agent-list-files-perms");
    std::fs::write(root.join("visible.txt"), "ok").unwrap();
    let restricted = root.join("restricted");
    std::fs::create_dir_all(restricted.join("nested")).unwrap();
    std::fs::write(restricted.join("nested/secret.txt"), "hidden").unwrap();
    std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o000)).unwrap();

    let tool = ListFilesTool::new(ToolContext::new(root.clone(), false).unwrap(), 100);
    let output = rig::tool::Tool::call(
        &tool,
        ListFilesArgs {
            path: Some(".".to_string()),
            recursive: true,
            max_entries: 100,
        },
    )
    .await
    .unwrap();

    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    let entries = value["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["path"] == "visible.txt"),
        "{output}"
    );

    std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn list_files_ignores_common_generated_directories_by_default() {
    let root = temp_root("defra-agent-list-files-ignored");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn hi() {}\n").unwrap();
    std::fs::write(root.join("target/debug/app"), "compiled").unwrap();
    let tool = ListFilesTool::new(ToolContext::new(root.clone(), false).unwrap(), 100);

    let output = rig::tool::Tool::call(
        &tool,
        ListFilesArgs {
            path: Some(".".to_string()),
            recursive: true,
            max_entries: 100,
        },
    )
    .await
    .unwrap();

    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    let entries = value["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["path"] == "src"),
        "{output}"
    );
    assert!(
        entries.iter().all(|entry| entry["path"] != "target"),
        "{output}"
    );
}

#[tokio::test]
async fn glob_returns_structured_json_matches() {
    let root = temp_root("defra-agent-glob");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("target/debug/main.rs"), "generated\n").unwrap();
    let tool = GlobTool::new(ToolContext::new(root.clone(), false).unwrap(), 100);

    let output = rig::tool::Tool::call(
        &tool,
        GlobArgs {
            pattern: "**/*.rs".to_string(),
            path: Some(".".to_string()),
            max_matches: 100,
        },
    )
    .await
    .unwrap();

    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    let matches = value["matches"].as_array().unwrap();
    assert!(
        matches.iter().any(|entry| entry["path"] == "src/main.rs"),
        "{output}"
    );
    assert!(matches
        .iter()
        .all(|entry| entry["path"] != "target/debug/main.rs"));
}

#[tokio::test]
async fn grep_returns_structured_json_matches() {
    let root = temp_root("defra-agent-grep");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    let tool = GrepTool::new(ToolContext::new(root.clone(), false).unwrap(), 100);

    let output = rig::tool::Tool::call(
        &tool,
        GrepArgs {
            pattern: "println".to_string(),
            path: Some(".".to_string()),
            case_sensitive: true,
            max_matches: 100,
        },
    )
    .await
    .unwrap();

    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["files_with_matches"], 1);
    let matches = value["matches"].as_array().unwrap();
    assert_eq!(matches[0]["path"], "src/main.rs");
    assert_eq!(matches[0]["line_number"], 2);
    assert!(matches[0]["preview"]
        .as_str()
        .unwrap()
        .contains("println!(\"hello\")"));
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
