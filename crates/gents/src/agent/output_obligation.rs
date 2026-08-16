use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::document_config::OutputObligationDecision;
use crate::graphql::{
    canonical_positive_count, escape_graphql_string, graphql_with_transaction_retry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveOutputObligation {
    pub(crate) tool_name: String,
    pub(crate) contract: crate::document_config::WriteToolOutputObligation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnmetOutputObligation {
    tool_name: String,
    minimum_writes: usize,
    completed_writes: usize,
    expected_writes: Option<usize>,
    expected_count_field: Option<String>,
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
    args: String,
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

    pub(crate) async fn unmet(&self) -> Result<Vec<UnmetOutputObligation>> {
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
                    args
                }}
            }}"#,
            escape_graphql_string(&self.request_doc_id),
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "loading completed output writes")
                .await?;
        let rows: Vec<CompletedWriteRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let mut writes = HashMap::<String, Vec<CompletedWriteRow>>::new();
        for row in rows {
            writes.entry(row.tool_name.clone()).or_default().push(row);
        }

        let mut unmet = Vec::new();
        for obligation in &self.obligations {
            let completed = writes
                .get(&obligation.tool_name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let expected = expected_write_count(obligation, completed)?;
            match obligation
                .contract
                .decision(completed.len(), expected, true)
            {
                OutputObligationDecision::Continue => unmet.push(UnmetOutputObligation {
                    tool_name: obligation.tool_name.clone(),
                    minimum_writes: obligation.contract.minimum_writes,
                    completed_writes: completed.len(),
                    expected_writes: expected,
                    expected_count_field: obligation.contract.expected_count_field.clone(),
                }),
                OutputObligationDecision::Complete => {}
                OutputObligationDecision::Reject => {
                    bail!(
                        "output obligation for `{}` has {} completed writes but declares expected count {:?} with minimum {}",
                        obligation.tool_name,
                        completed.len(),
                        expected,
                        obligation.contract.minimum_writes,
                    );
                }
            }
        }
        Ok(unmet)
    }
}

fn expected_write_count(
    obligation: &ActiveOutputObligation,
    completed: &[CompletedWriteRow],
) -> Result<Option<usize>> {
    let Some(field) = obligation.contract.expected_count_field.as_deref() else {
        return Ok(None);
    };
    let mut expected = None;
    for row in completed {
        let args: serde_json::Value = serde_json::from_str(&row.args).map_err(|error| {
            anyhow::anyhow!(
                "completed `{}` write has invalid durable arguments: {error}",
                obligation.tool_name
            )
        })?;
        let value = args.get(field).ok_or_else(|| {
            anyhow::anyhow!(
                "completed `{}` write is missing expected_count_field `{field}`",
                obligation.tool_name
            )
        })?;
        let count = canonical_positive_count(
            value,
            crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS,
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "completed `{}` write expected_count_field `{field}` must be a canonical positive integer <= {}",
                obligation.tool_name,
                crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS,
            )
        })?;
        if expected.is_some_and(|prior| prior != count) {
            bail!(
                "completed `{}` writes disagree on expected_count_field `{field}`",
                obligation.tool_name
            );
        }
        expected = Some(count);
    }
    Ok(expected)
}

pub(crate) fn continuation_message(obligations: &[UnmetOutputObligation]) -> String {
    let requirements = obligations
        .iter()
        .map(|obligation| {
            if let Some(expected) = obligation.expected_writes {
                format!(
                    "`{}` exactly {expected} total time(s) ({} completed, {} remaining)",
                    obligation.tool_name,
                    obligation.completed_writes,
                    expected.saturating_sub(obligation.completed_writes),
                )
            } else if let Some(field) = &obligation.expected_count_field {
                format!(
                    "`{}` at least once to declare the exact closed-set size in `{field}` ({} completed)",
                    obligation.tool_name, obligation.completed_writes
                )
            } else {
                format!(
                    "`{}` at least {} total time(s) ({} completed)",
                    obligation.tool_name, obligation.minimum_writes, obligation.completed_writes
                )
            }
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
                expected_count_field: None,
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
                expected_count_field: None,
            },
        };
        let gate =
            OutputObligationGate::new(node.clone(), "request-doc-output", vec![obligation.clone()]);

        let unmet = gate.unmet().await.unwrap();
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].tool_name, obligation.tool_name);
        assert_eq!(unmet[0].completed_writes, 0);

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

    async fn complete_write(
        node: Arc<EmbeddedNode>,
        request_doc_id: &str,
        call_id: &str,
        sequence: u32,
        args: &str,
    ) {
        let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::new(
            node,
            format!("request-{request_doc_id}"),
            format!("session-{request_doc_id}"),
            "did:test:output".to_string(),
            call_id.to_string(),
            sequence,
            "write_result".to_string(),
            args.to_string(),
            chrono::Utc::now() + chrono::Duration::minutes(1),
        )
        .with_request_doc_id(Some(request_doc_id.to_string()));
        lifecycle.start_running().await.unwrap();
        lifecycle.complete("created Result abc").await.unwrap();
    }

    #[tokio::test]
    async fn dynamic_count_blocks_until_the_durable_closed_set_is_complete() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let gate = OutputObligationGate::new(
            node.clone(),
            "request-doc-dynamic",
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::document_config::WriteToolOutputObligation {
                    scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                },
            }],
        );

        let initial = gate.unmet().await.unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].expected_writes, None);
        complete_write(
            node.clone(),
            "request-doc-dynamic",
            "call-dynamic-1",
            1,
            r#"{"expected_total":"3"}"#,
        )
        .await;
        let partial = gate.unmet().await.unwrap();
        assert_eq!(partial[0].completed_writes, 1);
        assert_eq!(partial[0].expected_writes, Some(3));
        assert!(continuation_message(&partial).contains("2 remaining"));

        for (sequence, call_id) in [(2, "call-dynamic-2"), (3, "call-dynamic-3")] {
            complete_write(
                node.clone(),
                "request-doc-dynamic",
                call_id,
                sequence,
                r#"{"expected_total":"3"}"#,
            )
            .await;
        }
        assert!(gate.unmet().await.unwrap().is_empty());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn dynamic_count_rejects_inconsistent_durable_members() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let gate = OutputObligationGate::new(
            node.clone(),
            "request-doc-inconsistent",
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::document_config::WriteToolOutputObligation {
                    scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                },
            }],
        );
        complete_write(
            node.clone(),
            "request-doc-inconsistent",
            "call-inconsistent-1",
            1,
            r#"{"expected_total":"2"}"#,
        )
        .await;
        complete_write(
            node.clone(),
            "request-doc-inconsistent",
            "call-inconsistent-2",
            2,
            r#"{"expected_total":"3"}"#,
        )
        .await;

        assert!(gate
            .unmet()
            .await
            .unwrap_err()
            .to_string()
            .contains("disagree"));
        node.shutdown().await;
    }
}
