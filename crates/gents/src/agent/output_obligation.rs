use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::document_config::OutputObligationDecision;
use crate::graphql::{
    canonical_positive_count, escape_graphql_string, graphql_with_transaction_retry,
};

// The gate seam (the trait the loop calls) and the unmet-obligation record
// moved to gents-loop (G-1); this module keeps the DefraDB-backed check.
use gents_loop::output_obligation::OutputObligationCheck;
pub(crate) use gents_loop::output_obligation::{continuation_message, UnmetOutputObligation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveOutputObligation {
    pub(crate) tool_name: String,
    pub(crate) contract: crate::document_config::WriteToolOutputObligation,
}

fn active_for_request(
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
    request_doc_ids: Vec<String>,
    obligations: Vec<ActiveOutputObligation>,
}

#[derive(Debug, Deserialize)]
struct CompletedWriteRow {
    tool_name: String,
    args: String,
}

#[derive(Deserialize)]
struct CompletedWriteIdentity {
    #[serde(rename = "_docID")]
    doc_id: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_doc_id: String,
    tool_name: String,
}

impl OutputObligationGate {
    #[cfg(test)]
    pub(crate) fn new(
        node: Arc<EmbeddedNode>,
        request_doc_id: impl Into<String>,
        obligations: Vec<ActiveOutputObligation>,
    ) -> Self {
        Self {
            node,
            request_doc_ids: vec![request_doc_id.into()],
            obligations,
        }
    }

    /// Use the existing configured tool contracts, with trigger activation and
    /// completed writes inherited through authenticated Goal ancestry.
    pub(crate) async fn for_request(
        node: Arc<EmbeddedNode>,
        request: &crate::watcher::AgentRequest,
        configured: &[(String, crate::document_config::WriteToolOutputObligation)],
    ) -> Result<Option<Self>> {
        if configured.is_empty() {
            return Ok(None);
        }
        let mut request_doc_ids = vec![request.doc_id.clone()];
        let mut triggered = request.has_automated_trigger_lineage();
        if request.caused_by_trigger_kind.as_deref() == Some(crate::goal::GOAL_TRIGGER_KIND) {
            let query = format!(
                r#"{{ AgentRequest(filter: {{ agent_did: {{ _eq: "{}" }},
                    session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                escape_graphql_string(&request.agent_did),
                escape_graphql_string(&request.session_id),
                crate::request_admission::SIGNED_REQUEST_FIELDS,
            );
            let response = graphql_with_transaction_retry(
                &node,
                &query,
                "loading output obligation request ancestry",
            )
            .await?;
            let rows: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(
                response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .cloned()
                    .context("output obligation ancestry query omitted requests")?,
            )?;
            let members = crate::goal::authenticated_goal_request_members(
                &request.agent_did,
                &request.session_id,
                &request.doc_id,
                &rows,
            )?;
            triggered = crate::watcher::AgentRequest::try_from(members.entry.clone())?
                .has_automated_trigger_lineage();
            request_doc_ids = members.member_doc_ids;
        }
        let obligations = active_for_request(configured, triggered);
        Ok((!obligations.is_empty()).then_some(Self {
            node,
            request_doc_ids,
            obligations,
        }))
    }

    pub(crate) async fn unmet(&self) -> Result<Vec<UnmetOutputObligation>> {
        if self.obligations.is_empty() {
            return Ok(Vec::new());
        }
        if self.request_doc_ids.is_empty()
            || self.request_doc_ids.iter().any(|id| id.trim().is_empty())
        {
            bail!("output obligations require a physical request document id");
        }

        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        request_doc_id: {{ _in: {} }},
                        lifecycle_state: {{ _eq: "completed" }}
                    }}
                ) {{
                    _docID agent_did requester_did session_id request_doc_id
                    tool_name
                }}
            }}"#,
            gents_protocol::graphql::graphql_string_list_literal(&self.request_doc_ids),
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "loading completed output writes")
                .await?;
        let rows: Vec<CompletedWriteIdentity> = serde_json::from_value(
            response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentToolCall"))
                .cloned()
                .context("completed output query omitted AgentToolCall rows")?,
        )?;
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        let mut sessions = HashMap::new();
        let mut requests = HashMap::new();
        let mut writes = HashMap::<String, Vec<CompletedWriteRow>>::new();
        for row in rows {
            if !self
                .obligations
                .iter()
                .any(|obligation| obligation.tool_name == row.tool_name)
            {
                continue;
            }
            anyhow::ensure!(
                !row.doc_id.trim().is_empty() && self.request_doc_ids.contains(&row.request_doc_id),
                "completed output write lacks exact request membership"
            );
            if !requests.contains_key(&row.request_doc_id) {
                let request = crate::request_binding::load_agent_request_by_doc_id(
                    &self.node,
                    &row.request_doc_id,
                )
                .await?
                .context("completed output write references a missing request")?;
                requests.insert(row.request_doc_id.clone(), request);
            }
            let request = &requests[&row.request_doc_id];
            anyhow::ensure!(
                request.agent_did == row.agent_did
                    && request.session_id == row.session_id
                    && request.requester_did == row.requester_did,
                "completed output write crossed its physical request ownership scope"
            );
            let scope = (
                row.agent_did.clone(),
                row.session_id.clone(),
                row.requester_did.clone(),
            );
            if !sessions.contains_key(&scope) {
                let calls = crate::run_timeline_fetch::load_session_tool_calls(
                    &access,
                    &row.agent_did,
                    &row.session_id,
                    row.requester_did.as_deref(),
                )
                .await?;
                sessions.insert(scope.clone(), calls);
            }
            let matches = sessions[&scope]
                .iter()
                .filter(|call| call.doc_id.as_deref() == Some(row.doc_id.as_str()))
                .collect::<Vec<_>>();
            anyhow::ensure!(
                matches.len() == 1,
                "completed output write has no unique physical tool binding"
            );
            let call = matches[0];
            anyhow::ensure!(
                call.request_doc_id.as_deref() == Some(row.request_doc_id.as_str())
                    && call.tool_name == row.tool_name
                    && call.lifecycle_state.as_deref() == Some("completed"),
                "completed output write changed its accepted identity or terminal state"
            );
            writes
                .entry(row.tool_name.clone())
                .or_default()
                .push(CompletedWriteRow {
                    tool_name: row.tool_name,
                    args: call.args.clone(),
                });
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

impl OutputObligationCheck for OutputObligationGate {
    fn unmet<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<UnmetOutputObligation>>> + Send + 'a>,
    > {
        Box::pin(OutputObligationGate::unmet(self))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;

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

    /// The gate counts ONLY durable completed writes published through the
    /// canonical accepted path under an actual claimed request, so this
    /// fixture drives a signed automated-trigger request through the
    /// production claim, native publication, and terminal tool delivery —
    /// the same pattern as the logical fixtures — instead of fabricating a
    /// ToolCall lifecycle that no publisher ever accepted.
    #[tokio::test]
    async fn durable_completed_writes_satisfy_the_gate() {
        use crate::identity::KeyIdentity;
        use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
        use crate::streaming::DefraStreamWriter;
        use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy};
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "request-output-gate",
            identity.did(),
            identity.did(),
            "output-gate-behavior",
            "output-gate-session",
            "Write the result",
            "scheduled",
            "2026-09-05T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(
                identity.did(),
                "output-gate-trigger",
            ),
        );
        create.caused_by_trigger_kind = Some("event".into());
        create.caused_by_trigger_id = Some("output-gate-trigger".into());
        create.caused_by_trigger_doc_id = Some("original-trigger-doc".into());
        create.caused_by_source_doc_id = Some("original-area-doc".into());
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                crate::graphql::escape_graphql_string(&create.request_id),
                crate::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await;
        assert!(!data.has_errors(), "{:?}", data.errors);
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(data.data.unwrap()["AgentRequest"][0].clone()).unwrap();
        let request = crate::watcher::AgentRequest::try_from(row).unwrap();

        let mut lifecycle = RequestLifecycle::new_with_agent_did(
            node.clone(),
            "general",
            &request.agent_did,
            request.clone(),
            60,
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        let writer = DefraStreamWriter::new(
            node.clone(),
            "did:test:test",
            std::time::Duration::from_millis(1),
        );
        lifecycle.begin_owned_execution(&writer).await.unwrap();

        let obligation = ActiveOutputObligation {
            tool_name: "write_result".to_string(),
            contract: crate::document_config::WriteToolOutputObligation {
                scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: None,
            },
        };
        let gate = OutputObligationGate::new(
            node.clone(),
            lifecycle.request().doc_id.clone(),
            vec![obligation.clone()],
        );

        let unmet = gate.unmet().await.unwrap();
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].tool_name, obligation.tool_name);
        assert_eq!(unmet[0].completed_writes, 0);

        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let message = gents_protocol::message::Message::Assistant {
            id: Some("provider-message".into()),
            content: vec![gents_protocol::message::AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    id: "native-write-result".into(),
                    call_id: None,
                    function: gents_protocol::message::ToolFunction::new(
                        "write_result".into(),
                        serde_json::json!({"expected_total": 1}),
                    ),
                    signature: None,
                    additional_params: None,
                },
            )],
        };
        let mut published = writer
            .publish_native_turn(&lifecycle, 0, 0, &message)
            .await
            .unwrap();
        assert_eq!(
            published.accepted_tools.len(),
            1,
            "canonical publication must accept exactly one tool call"
        );
        let accepted = published.accepted_tools.pop().unwrap();

        let mut tool = crate::tool_call_lifecycle::ToolCallLifecycle::from_accepted(
            node.clone(),
            lifecycle.request().agent_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            lifecycle
                .claimed_deadline_at()
                .expect("claimed request deadline"),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
        )
        .unwrap();
        tool.start_running().await.unwrap();
        tool.complete("created Result abc").await.unwrap();

        assert!(gate.unmet().await.unwrap().is_empty());
        node.shutdown().await;
    }

    /// A signed automated-trigger request the gate can resolve against: the
    /// trigger scope activates on the request's automated lineage, and the
    /// physical doc ID the gate counts is the persisted request's doc ID.
    async fn automated_trigger_request(
        node: &Arc<EmbeddedNode>,
        request_id: &str,
    ) -> crate::watcher::AgentRequest {
        use crate::identity::KeyIdentity;
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            "output-gate-behavior",
            "output-gate-session",
            "Write the result",
            "scheduled",
            "2026-09-05T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(
                identity.did(),
                "output-gate-trigger",
            ),
        );
        create.caused_by_trigger_kind = Some("event".into());
        create.caused_by_trigger_id = Some("output-gate-trigger".into());
        create.caused_by_trigger_doc_id = Some("original-trigger-doc".into());
        create.caused_by_source_doc_id = Some("original-area-doc".into());
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                crate::graphql::escape_graphql_string(&create.request_id),
                crate::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await;
        assert!(!data.has_errors(), "{:?}", data.errors);
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(data.data.unwrap()["AgentRequest"][0].clone()).unwrap();
        crate::watcher::AgentRequest::try_from(row).unwrap()
    }

    /// The gate counts ONLY durable completed writes published through the
    /// canonical accepted path under an actual claimed request, so this
    /// helper publishes ONE accepted native tool call turn under the
    /// caller's claimed request lifecycle and drives it to terminal
    /// completion — the same pattern as the logical fixtures — instead of
    /// fabricating a ToolCall lifecycle that no publisher ever accepted.
    async fn complete_write(
        node: &Arc<EmbeddedNode>,
        writer: &crate::streaming::DefraStreamWriter,
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
        turn: usize,
        tool_call_id: &str,
        arguments: &serde_json::Value,
    ) {
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                turn,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let message = gents_protocol::message::Message::Assistant {
            id: Some("provider-message".into()),
            content: vec![gents_protocol::message::AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    id: tool_call_id.into(),
                    call_id: None,
                    function: gents_protocol::message::ToolFunction::new(
                        "write_result".into(),
                        arguments.clone(),
                    ),
                    signature: None,
                    additional_params: None,
                },
            )],
        };
        let mut published = writer
            .publish_native_turn(lifecycle, turn, 0, &message)
            .await
            .unwrap();
        assert_eq!(
            published.accepted_tools.len(),
            1,
            "canonical publication must accept exactly one tool call"
        );
        let accepted = published.accepted_tools.pop().unwrap();
        let mut tool = crate::tool_call_lifecycle::ToolCallLifecycle::from_accepted(
            node.clone(),
            lifecycle.request().agent_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            lifecycle
                .claimed_deadline_at()
                .expect("claimed request deadline"),
            crate::tool_call_lifecycle::AwaitMode::Foreground,
            crate::tool_call_lifecycle::CancelPolicy::Cascade,
        )
        .unwrap();
        tool.start_running().await.unwrap();
        tool.complete("persisted output").await.unwrap();
    }

    #[tokio::test]
    async fn dynamic_count_blocks_until_the_durable_closed_set_is_complete() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let request = automated_trigger_request(&node, "request-doc-dynamic").await;
        let gate = OutputObligationGate::new(
            node.clone(),
            request.doc_id.clone(),
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
        let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_agent_did(
            node.clone(),
            "general",
            &request.agent_did,
            request.clone(),
            60,
        );
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            "did:test:test",
            std::time::Duration::from_millis(1),
        );
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        complete_write(
            &node,
            &writer,
            &mut lifecycle,
            0,
            "native-dynamic-1",
            &serde_json::json!({"expected_total": "3"}),
        )
        .await;
        let partial = gate.unmet().await.unwrap();
        assert_eq!(partial[0].completed_writes, 1);
        assert_eq!(partial[0].expected_writes, Some(3));
        assert!(continuation_message(&partial).contains("2 remaining"));

        for (turn, call_id) in [(1, "native-dynamic-2"), (2, "native-dynamic-3")] {
            complete_write(
                &node,
                &writer,
                &mut lifecycle,
                turn,
                call_id,
                &serde_json::json!({"expected_total": "3"}),
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
        let request = automated_trigger_request(&node, "request-doc-inconsistent").await;
        let gate = OutputObligationGate::new(
            node.clone(),
            request.doc_id.clone(),
            vec![ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::document_config::WriteToolOutputObligation {
                    scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                },
            }],
        );
        let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_agent_did(
            node.clone(),
            "general",
            &request.agent_did,
            request.clone(),
            60,
        );
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            "did:test:test",
            std::time::Duration::from_millis(1),
        );
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        for (turn, call_id, expected) in [
            (0, "native-inconsistent-1", 2),
            (1, "native-inconsistent-2", 3),
        ] {
            complete_write(
                &node,
                &writer,
                &mut lifecycle,
                turn,
                call_id,
                &serde_json::json!({"expected_total": expected}),
            )
            .await;
        }

        assert!(gate
            .unmet()
            .await
            .unwrap_err()
            .to_string()
            .contains("disagree"));
        node.shutdown().await;
    }
}

#[cfg(test)]
#[path = "output_obligation/logical_tests.rs"]
mod logical_tests;
