use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveOutputObligation {
    pub(crate) tool_name: String,
    pub(crate) contract: crate::document_config::WriteToolOutputObligation,
}

pub(crate) fn active_for_request(
    configured: &[(String, crate::document_config::WriteToolOutputObligation)],
    has_automated_trigger_lineage: bool,
) -> Vec<ActiveOutputObligation> {
    configured
        .iter()
        .filter(|(_, obligation)| obligation.applies_to(has_automated_trigger_lineage))
        .map(|(tool_name, obligation)| ActiveOutputObligation {
            tool_name: tool_name.clone(),
            contract: obligation.clone(),
        })
        .collect()
}

#[derive(Clone)]
pub(crate) struct OutputObligationGate {
    node: Arc<EmbeddedNode>,
    request_doc_id: String,
    obligations: Vec<ActiveOutputObligation>,
}

#[derive(Debug, Deserialize)]
struct CompletedWriteRow {
    tool_name: String,
}

impl OutputObligationGate {
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        request_doc_id: impl Into<String>,
        obligations: Vec<ActiveOutputObligation>,
    ) -> Self {
        Self {
            node,
            request_doc_id: request_doc_id.into(),
            obligations,
        }
    }

    pub(crate) async fn unmet(&self) -> Result<Vec<ActiveOutputObligation>> {
        if self.obligations.is_empty() {
            return Ok(Vec::new());
        }
        if self.request_doc_id.trim().is_empty() {
            bail!("output obligations require a physical request document id");
        }

        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        request_doc_id: {{ _eq: "{}" }},
                        lifecycle_state: {{ _eq: "completed" }}
                    }}
                ) {{
                    tool_name
                }}
            }}"#,
            escape_graphql_string(&self.request_doc_id),
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            bail!(
                "loading completed output writes failed: {:?}",
                response.errors
            );
        }
        let rows: Vec<CompletedWriteRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let mut counts = HashMap::<String, usize>::new();
        for row in rows {
            *counts.entry(row.tool_name).or_default() += 1;
        }

        Ok(self
            .obligations
            .iter()
            .filter(|obligation| {
                !obligation.contract.is_satisfied(
                    counts
                        .get(&obligation.tool_name)
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .cloned()
            .collect())
    }
}

pub(crate) fn continuation_message(obligations: &[ActiveOutputObligation]) -> String {
    let requirements = obligations
        .iter()
        .map(|obligation| {
            format!(
                "`{}` at least {} time(s)",
                obligation.tool_name, obligation.contract.minimum_writes
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "The request cannot complete yet because its configured output obligation is unmet. \
         Complete the required durable write before answering: {requirements}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_scope_follows_automated_trigger_lineage() {
        let configured = vec![(
            "write_result".to_string(),
            crate::document_config::WriteToolOutputObligation {
                scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
            },
        )];
        assert!(active_for_request(&configured, false).is_empty());
        assert_eq!(
            active_for_request(&configured, true),
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: configured[0].1.clone(),
            }]
        );
    }

    #[tokio::test]
    async fn durable_completed_writes_satisfy_the_gate() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let obligation = ActiveOutputObligation {
            tool_name: "write_result".to_string(),
            contract: crate::document_config::WriteToolOutputObligation {
                scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
            },
        };
        let gate =
            OutputObligationGate::new(node.clone(), "request-doc-output", vec![obligation.clone()]);

        assert_eq!(gate.unmet().await.unwrap(), vec![obligation]);

        let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::new(
            node.clone(),
            "request-output".to_string(),
            "session-output".to_string(),
            "did:test:output".to_string(),
            "call-output".to_string(),
            1,
            "write_result".to_string(),
            "{}".to_string(),
            chrono::Utc::now() + chrono::Duration::minutes(1),
        )
        .with_request_doc_id(Some("request-doc-output".to_string()));
        lifecycle.start_running().await.unwrap();

        assert_eq!(gate.unmet().await.unwrap().len(), 1);
        lifecycle.complete("created Result abc").await.unwrap();
        assert!(gate.unmet().await.unwrap().is_empty());
        node.shutdown().await;
    }
}
