use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{Tool, ToolDyn};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::graphql::escape_graphql_string;

pub const MEMORY_TOOL_NAME: &str = "memory";

const MAX_KEY_CHARS: usize = 256;
const MAX_VALUE_CHARS: usize = 32_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryAction {
    Read,
    Write,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryParams {
    pub action: MemoryAction,
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryRow {
    memory_id: String,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryOutput {
    action: &'static str,
    key: String,
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

#[derive(Debug)]
pub struct MemoryToolError(anyhow::Error);

impl std::fmt::Display for MemoryToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for MemoryToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for MemoryToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Clone)]
pub struct MemoryTool {
    node: Arc<EmbeddedNode>,
    agent_did: String,
}

impl MemoryTool {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        Self {
            node,
            agent_did: agent_did.into(),
        }
    }
}

impl Tool for MemoryTool {
    const NAME: &'static str = MEMORY_TOOL_NAME;

    type Error = MemoryToolError;
    type Args = MemoryParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read or write this agent's persistent cross-session memory. \
                Memory is a per-agent key-value store scoped to the running agent DID; \
                it cannot read or write arbitrary DefraDB documents or another agent's memory."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read", "write"],
                        "description": "Use \"read\" to fetch a key or \"write\" to store a key."
                    },
                    "key": {
                        "type": "string",
                        "description": "Memory key. Trimmed, non-empty, at most 256 characters, no control characters."
                    },
                    "value": {
                        "type": "string",
                        "description": "Value to store. Required for action=\"write\"; at most 32,000 characters."
                    }
                },
                "required": ["action", "key"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let agent_did = normalize_agent_did(&self.agent_did)?;
        let key = normalize_key(&args.key)?;

        let output = match args.action {
            MemoryAction::Read => read_memory(&self.node, &agent_did, &key).await?,
            MemoryAction::Write => {
                let value = normalize_value(
                    args.value
                        .as_deref()
                        .context("memory write requires `value`")?,
                )?;
                write_memory(&self.node, &agent_did, &key, &value).await?
            }
        };

        serde_json::to_string_pretty(&output)
            .map_err(|error| MemoryToolError(anyhow!("failed to serialize memory output: {error}")))
    }
}

pub fn build_memory_tool(
    node: Arc<EmbeddedNode>,
    agent_did: impl Into<String>,
) -> Box<dyn ToolDyn> {
    Box::new(MemoryTool::new(node, agent_did))
}

fn normalize_agent_did(agent_did: &str) -> Result<String> {
    let agent_did = agent_did.trim();
    if agent_did.is_empty() {
        bail!("memory tool requires a running agent DID");
    }
    Ok(agent_did.to_string())
}

fn normalize_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() {
        bail!("memory key must be non-empty");
    }
    if key.chars().count() > MAX_KEY_CHARS {
        bail!("memory key exceeds {MAX_KEY_CHARS} characters");
    }
    if key.chars().any(char::is_control) {
        bail!("memory key must not contain control characters");
    }
    Ok(key.to_string())
}

fn normalize_value(value: &str) -> Result<String> {
    if value.chars().count() > MAX_VALUE_CHARS {
        bail!("memory value exceeds {MAX_VALUE_CHARS} characters");
    }
    Ok(value.to_string())
}

fn memory_id(agent_did: &str, key: &str) -> String {
    format!("{}:{}{}", agent_did.len(), agent_did, key)
}

async fn read_memory(node: &EmbeddedNode, agent_did: &str, key: &str) -> Result<MemoryOutput> {
    let memory_id = escape_graphql_string(&memory_id(agent_did, key));
    let query = format!(
        r#"{{
            AgentMemory(filter: {{ memory_id: {{ _eq: "{memory_id}" }} }}, limit: 1) {{
                memory_id
                agent_did
                key
                value
                updated_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!("reading agent memory failed: {:?}", resp.errors);
    }
    tracing::debug!(agent_did, key, "agent memory read");

    let row = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMemory"))
        .and_then(|value| serde_json::from_value::<Vec<MemoryRow>>(value.clone()).ok())
        .and_then(|mut rows| rows.pop());

    Ok(match row {
        Some(row) => MemoryOutput {
            action: "read",
            key: row.key.unwrap_or_else(|| key.to_string()),
            found: true,
            value: row.value,
            updated_at: row.updated_at,
        },
        None => MemoryOutput {
            action: "read",
            key: key.to_string(),
            found: false,
            value: None,
            updated_at: None,
        },
    })
}

async fn write_memory(
    node: &EmbeddedNode,
    agent_did: &str,
    key: &str,
    value: &str,
) -> Result<MemoryOutput> {
    let updated_at = chrono::Utc::now().to_rfc3339();
    let output_key = key.to_string();
    let output_value = value.to_string();
    let output_updated_at = updated_at.clone();
    let escaped_memory_id = escape_graphql_string(&memory_id(agent_did, key));
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_key = escape_graphql_string(key);
    let escaped_value = escape_graphql_string(value);
    let escaped_updated_at = escape_graphql_string(&updated_at);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentMemory(
                filter: {{ memory_id: {{ _eq: "{escaped_memory_id}" }} }},
                add: {{
                    memory_id: "{escaped_memory_id}",
                    agent_did: "{escaped_agent_did}",
                    key: "{escaped_key}",
                    value: "{escaped_value}",
                    updated_at: "{escaped_updated_at}"
                }},
                update: {{
                    value: "{escaped_value}",
                    updated_at: "{escaped_updated_at}"
                }}
            ) {{ _docID }}
        }}"#
    );
    crate::graphql::graphql_mutation_with_transaction_retry(node, &mutation, "write agent memory")
        .await?;
    tracing::debug!(
        agent_did,
        key,
        value_chars = value.chars().count(),
        "agent memory write"
    );

    Ok(MemoryOutput {
        action: "write",
        key: output_key,
        found: true,
        value: Some(output_value),
        updated_at: Some(output_updated_at),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::llm::tool::Tool;
    use serde_json::Value;

    use super::*;

    async fn seeded_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        node
    }

    #[tokio::test]
    async fn memory_roundtrips_across_tool_instances_for_same_agent() {
        let node = seeded_node().await;
        let writer = MemoryTool::new(node.clone(), "did:key:z-memory");
        let reader = MemoryTool::new(node, "did:key:z-memory");

        Tool::call(
            &writer,
            MemoryParams {
                action: MemoryAction::Write,
                key: "project".to_string(),
                value: Some("gents".to_string()),
            },
        )
        .await
        .expect("write memory");

        let output = Tool::call(
            &reader,
            MemoryParams {
                action: MemoryAction::Read,
                key: "project".to_string(),
                value: None,
            },
        )
        .await
        .expect("read memory");
        let parsed: Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["found"], true);
        assert_eq!(parsed["value"], "gents");
    }

    #[tokio::test]
    async fn memory_is_scoped_to_agent_did() {
        let node = seeded_node().await;
        let first_agent = MemoryTool::new(node.clone(), "did:key:z-first");
        let second_agent = MemoryTool::new(node, "did:key:z-second");

        Tool::call(
            &first_agent,
            MemoryParams {
                action: MemoryAction::Write,
                key: "shared-key".to_string(),
                value: Some("first".to_string()),
            },
        )
        .await
        .expect("write memory");

        let output = Tool::call(
            &second_agent,
            MemoryParams {
                action: MemoryAction::Read,
                key: "shared-key".to_string(),
                value: None,
            },
        )
        .await
        .expect("read memory");
        let parsed: Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["found"], false);
    }

    #[tokio::test]
    async fn rejects_malformed_memory_writes() {
        let node = seeded_node().await;
        let tool = MemoryTool::new(node, "did:key:z-memory");

        let error = Tool::call(
            &tool,
            MemoryParams {
                action: MemoryAction::Write,
                key: " ".to_string(),
                value: Some("x".to_string()),
            },
        )
        .await
        .expect_err("empty keys must fail");
        assert!(error.to_string().contains("memory key must be non-empty"));

        let error = Tool::call(
            &tool,
            MemoryParams {
                action: MemoryAction::Write,
                key: "valid".to_string(),
                value: None,
            },
        )
        .await
        .expect_err("missing values must fail");
        assert!(error.to_string().contains("requires `value`"));
    }

    #[tokio::test]
    async fn rejects_oversized_key_and_value() {
        let node = seeded_node().await;
        let tool = MemoryTool::new(node, "did:key:z-memory");

        let error = Tool::call(
            &tool,
            MemoryParams {
                action: MemoryAction::Write,
                key: "k".repeat(MAX_KEY_CHARS + 1),
                value: Some("x".to_string()),
            },
        )
        .await
        .expect_err("oversized keys must fail");
        assert!(error.to_string().contains("memory key exceeds"));

        let error = Tool::call(
            &tool,
            MemoryParams {
                action: MemoryAction::Write,
                key: "valid".to_string(),
                value: Some("v".repeat(MAX_VALUE_CHARS + 1)),
            },
        )
        .await
        .expect_err("oversized values must fail");
        assert!(error.to_string().contains("memory value exceeds"));
    }

    #[tokio::test]
    async fn read_missing_key_for_same_agent_returns_not_found() {
        let node = seeded_node().await;
        let tool = MemoryTool::new(node, "did:key:z-memory");

        let output = Tool::call(
            &tool,
            MemoryParams {
                action: MemoryAction::Read,
                key: "never-written".to_string(),
                value: None,
            },
        )
        .await
        .expect("read memory");
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["found"], false);
        assert_eq!(parsed["value"], Value::Null);
    }

    #[tokio::test]
    async fn write_updates_value_in_place() {
        let node = seeded_node().await;
        let tool = MemoryTool::new(node, "did:key:z-memory");

        for value in ["first", "second"] {
            Tool::call(
                &tool,
                MemoryParams {
                    action: MemoryAction::Write,
                    key: "k".to_string(),
                    value: Some(value.to_string()),
                },
            )
            .await
            .expect("write memory");
        }

        let output = Tool::call(
            &tool,
            MemoryParams {
                action: MemoryAction::Read,
                key: "k".to_string(),
                value: None,
            },
        )
        .await
        .expect("read memory");
        let parsed: Value = serde_json::from_str(&output).unwrap();
        // upsert overwrites rather than duplicating: the re-written value wins.
        assert_eq!(parsed["found"], true);
        assert_eq!(parsed["value"], "second");
    }
}
