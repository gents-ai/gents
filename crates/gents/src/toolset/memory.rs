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
    #[serde(rename = "_docID")]
    doc_id: String,
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
    let row = load_memory_document_exact(node, agent_did, key).await?;
    tracing::debug!(agent_did, key, "agent memory read");

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

async fn load_memory_document_exact(
    node: &EmbeddedNode,
    agent_did: &str,
    key: &str,
) -> Result<Option<MemoryRow>> {
    let expected_memory_id = memory_id(agent_did, key);
    let escaped_memory_id = escape_graphql_string(&expected_memory_id);
    let query = format!(
        r#"{{
            AgentMemory(filter: {{ memory_id: {{ _eq: "{escaped_memory_id}" }} }}) {{
                _docID
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
    let rows: Vec<MemoryRow> = match resp.data.as_ref().and_then(|data| data.get("AgentMemory")) {
        Some(value) => serde_json::from_value(value.clone())
            .context("decoding complete AgentMemory logical match set")?,
        None => Vec::new(),
    };
    let row = crate::session::resolve_exact_logical_match(
        "AgentMemory",
        "memory_id",
        &expected_memory_id,
        rows,
        |row| row.doc_id.as_str(),
    )?;
    if let Some(row) = row.as_ref() {
        if row.memory_id != expected_memory_id {
            bail!(
                "AgentMemory logical key mismatch: queried memory_id={expected_memory_id} but _docID={} returned memory_id={}",
                row.doc_id,
                row.memory_id
            );
        }
        if row.agent_did.as_deref().map(str::trim) != Some(agent_did) {
            bail!(
                "AgentMemory immutable owner mismatch for memory_id={expected_memory_id}: _docID={} existing agent_did={:?} expected={agent_did}",
                row.doc_id,
                row.agent_did
            );
        }
        if row.key.as_deref() != Some(key) {
            bail!(
                "AgentMemory immutable key mismatch for memory_id={expected_memory_id}: _docID={} existing key={:?} expected={key}",
                row.doc_id,
                row.key
            );
        }
    }
    Ok(row)
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
    let existing = load_memory_document_exact(node, agent_did, key).await?;
    let (mutation_field, expected_doc_id, mutation) = if let Some(existing) = existing.as_ref() {
        let escaped_doc_id = escape_graphql_string(&existing.doc_id);
        (
            "update_AgentMemory",
            Some(existing.doc_id.as_str()),
            format!(
                r#"mutation {{
                update_AgentMemory(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        value: "{escaped_value}",
                        updated_at: "{escaped_updated_at}"
                    }}
                ) {{ _docID }}
            }}"#
            ),
        )
    } else {
        (
            "create_AgentMemory",
            None,
            format!(
                r#"mutation {{
                create_AgentMemory(input: {{
                    memory_id: "{escaped_memory_id}",
                    agent_did: "{escaped_agent_did}",
                    key: "{escaped_key}",
                    value: "{escaped_value}",
                    updated_at: "{escaped_updated_at}"
                }}) {{ _docID }}
            }}"#
            ),
        )
    };
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        bail!("writing agent memory failed: {:?}", resp.errors);
    }
    let returned_doc_id = exact_memory_mutation_doc_id(resp.data.as_ref(), mutation_field)?;
    if let Some(expected_doc_id) = expected_doc_id {
        if returned_doc_id != expected_doc_id {
            bail!(
                "AgentMemory exact update returned _docID={returned_doc_id}, expected {expected_doc_id}"
            );
        }
    }
    let verified = load_memory_document_exact(node, agent_did, key)
        .await?
        .context("AgentMemory disappeared after write")?;
    if verified.doc_id != returned_doc_id
        || verified.value.as_deref() != Some(value)
        || verified.updated_at.as_deref() != Some(updated_at.as_str())
    {
        bail!(
            "AgentMemory write verification failed for memory_id={}: mutation _docID={returned_doc_id}, observed _docID={} value_match={} timestamp_match={}",
            memory_id(agent_did, key),
            verified.doc_id,
            verified.value.as_deref() == Some(value),
            verified.updated_at.as_deref() == Some(updated_at.as_str())
        );
    }
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

fn exact_memory_mutation_doc_id(data: Option<&serde_json::Value>, field: &str) -> Result<String> {
    let add_field = field
        .strip_prefix("create_")
        .map(|collection| format!("add_{collection}"));
    let Some(value) = data.and_then(|data| {
        data.get(field)
            .or_else(|| add_field.as_deref().and_then(|field| data.get(field)))
    }) else {
        bail!("{field} returned no result");
    };
    let document_ids = if let Some(doc_id) = value.get("_docID").and_then(serde_json::Value::as_str)
    {
        vec![doc_id.to_string()]
    } else {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("_docID").and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    };
    match document_ids.as_slice() {
        [doc_id] if !doc_id.trim().is_empty() => Ok(doc_id.clone()),
        _ => bail!("{field} returned non-exact _docIDs={document_ids:?}"),
    }
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

    #[tokio::test]
    async fn duplicate_memory_logical_id_fails_closed_without_read_or_update() {
        // Deliberately omit the current unique index to reproduce an older or
        // replicated collection containing logical twins.
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(
            r#"
            type AgentMemory {
                memory_id: String
                agent_did: String
                key: String
                value: String
                updated_at: String
            }
            "#,
        )
        .await
        .unwrap();
        let agent_did = "did:key:z-memory-owner";
        let key = "durability";
        let logical_id = memory_id(agent_did, key);
        for (owner, stored_key, value) in [
            (agent_did, key, "first"),
            ("did:key:z-conflicting-owner", "conflicting-key", "second"),
        ] {
            let response = node
                .execute(&format!(
                    r#"mutation {{
                        create_AgentMemory(input: {{
                            memory_id: "{}"
                            agent_did: "{}"
                            key: "{}"
                            value: "{}"
                            updated_at: "2026-08-08T00:00:00Z"
                        }}) {{ _docID }}
                    }}"#,
                    escape_graphql_string(&logical_id),
                    escape_graphql_string(owner),
                    escape_graphql_string(stored_key),
                    escape_graphql_string(value),
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }

        for error in [
            read_memory(&node, agent_did, key)
                .await
                .expect_err("read must reject logical twins"),
            write_memory(&node, agent_did, key, "replacement")
                .await
                .expect_err("write must reject logical twins"),
        ] {
            assert!(
                error
                    .downcast_ref::<crate::session::LogicalDocumentResolutionError>()
                    .is_some_and(|error| matches!(
                        error,
                        crate::session::LogicalDocumentResolutionError::Conflict(_)
                    )),
                "expected typed AgentMemory conflict, got {error:#}"
            );
        }

        let response = node
            .execute(&format!(
                r#"{{
                    AgentMemory(filter: {{ memory_id: {{ _eq: "{}" }} }}) {{ value updated_at }}
                }}"#,
                escape_graphql_string(&logical_id)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let mut values = response.data.unwrap()["AgentMemory"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["value"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        values.sort();
        assert_eq!(values, ["first", "second"]);
    }

    #[tokio::test]
    async fn memory_singleton_rejects_immutable_owner_mismatch() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(
            r#"
            type AgentMemory {
                memory_id: String
                agent_did: String
                key: String
                value: String
                updated_at: String
            }
            "#,
        )
        .await
        .unwrap();
        let agent_did = "did:key:z-memory-owner";
        let key = "durability";
        let logical_id = memory_id(agent_did, key);
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentMemory(input: {{
                        memory_id: "{}"
                        agent_did: "did:key:z-foreign"
                        key: "wrong-key"
                        value: "original"
                        updated_at: "2026-08-08T00:00:00Z"
                    }}) {{ _docID }}
                }}"#,
                escape_graphql_string(&logical_id)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let error = write_memory(&node, agent_did, key, "replacement")
            .await
            .expect_err("a logical key must not authorize a different owner/key tuple");
        assert!(
            error.to_string().contains("immutable owner mismatch"),
            "{error:#}"
        );
        let response = node
            .execute(&format!(
                r#"{{
                    AgentMemory(filter: {{ memory_id: {{ _eq: "{}" }} }}) {{ value }}
                }}"#,
                escape_graphql_string(&logical_id)
            ))
            .await;
        assert_eq!(
            response.data.unwrap()["AgentMemory"][0]["value"],
            "original"
        );
    }

    #[tokio::test]
    async fn memory_singleton_rejects_immutable_key_mismatch() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(
            r#"
            type AgentMemory {
                memory_id: String
                agent_did: String
                key: String
                value: String
                updated_at: String
            }
            "#,
        )
        .await
        .unwrap();
        let agent_did = "did:key:z-memory-owner";
        let key = "durability";
        let logical_id = memory_id(agent_did, key);
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentMemory(input: {{
                        memory_id: "{}"
                        agent_did: "{}"
                        key: "wrong-key"
                        value: "original"
                        updated_at: "2026-08-08T00:00:00Z"
                    }}) {{ _docID }}
                }}"#,
                escape_graphql_string(&logical_id),
                escape_graphql_string(agent_did)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let error = write_memory(&node, agent_did, key, "replacement")
            .await
            .expect_err("a logical key must not authorize a different stored key");
        assert!(
            error.to_string().contains("immutable key mismatch"),
            "{error:#}"
        );
        let response = node
            .execute(&format!(
                r#"{{
                    AgentMemory(filter: {{ memory_id: {{ _eq: "{}" }} }}) {{ value }}
                }}"#,
                escape_graphql_string(&logical_id)
            ))
            .await;
        assert_eq!(
            response.data.unwrap()["AgentMemory"][0]["value"],
            "original"
        );
    }
}
