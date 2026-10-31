use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::documents::OutputObligationDecision;
use crate::store::graphql::{
    canonical_positive_count, escape_graphql_string, graphql_with_transaction_retry,
};

use gents_loop::output_obligation::OutputObligationCheck;
pub(crate) use gents_loop::output_obligation::{continuation_message, UnmetOutputObligation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveOutputObligation {
    pub(crate) tool_name: String,
    pub(crate) contract: crate::documents::WriteToolOutputObligation,
}

fn active_for_request(
    configured: &[(String, crate::documents::WriteToolOutputObligation)],
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
    args: String,
}

#[derive(Deserialize)]
struct CompletedWriteIdentity {
    #[serde(rename = "_docID")]
    doc_id: String,
    node_did: String,
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

    pub(crate) async fn for_request(
        node: Arc<EmbeddedNode>,
        request: &crate::request::watcher::AgentRequest,
        configured: &[(String, crate::documents::WriteToolOutputObligation)],
    ) -> Result<Option<Self>> {
        if configured.is_empty() {
            return Ok(None);
        }
        let mut request_doc_ids = vec![request.doc_id.clone()];
        let mut triggered = request.has_automated_trigger_lineage();
        if request.caused_by_trigger_kind.as_deref() == Some(crate::goal::GOAL_TRIGGER_KIND) {
            let query = format!(
                r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }},
                    session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                escape_graphql_string(&request.node_did),
                escape_graphql_string(&request.session_id),
                crate::request::admission::SIGNED_REQUEST_FIELDS,
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
                &request.node_did,
                &request.session_id,
                &request.doc_id,
                &rows,
            )?;
            triggered = crate::request::watcher::AgentRequest::try_from(members.entry.clone())?
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
                    _docID node_did requester_did session_id request_doc_id
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
        let access = crate::store::config_client::ConfigAccess::local(self.node.clone());
        let mut requests = RapidHashMap::new();
        let mut writes = RapidHashMap::<String, Vec<CompletedWriteRow>>::new();
        let mut scoped_rows = RapidHashMap::<(String, String, Option<String>), Vec<_>>::new();
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
                let request = crate::request::binding::load_agent_request_by_doc_id(
                    &self.node,
                    &row.request_doc_id,
                )
                .await?
                .context("completed output write references a missing request")?;
                requests.insert(row.request_doc_id.clone(), request);
            }
            let request = &requests[&row.request_doc_id];
            anyhow::ensure!(
                request.node_did == row.node_did
                    && request.session_id == row.session_id
                    && request.requester_did == row.requester_did,
                "completed output write crossed its physical request ownership scope"
            );
            scoped_rows
                .entry((
                    row.node_did.clone(),
                    row.session_id.clone(),
                    row.requester_did.clone(),
                ))
                .or_default()
                .push(row);
        }
        for ((node_did, session_id, requester_did), rows) in scoped_rows {
            let tool_doc_ids = rows
                .iter()
                .map(|row| row.doc_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let calls = crate::observe::run_timeline_fetch::load_accepted_tool_calls(
                &access,
                &node_did,
                &session_id,
                requester_did.as_deref(),
                &tool_doc_ids,
            )
            .await?;
            let mut calls_by_doc = RapidHashMap::<String, Vec<_>>::new();
            for call in calls {
                let doc_id = call
                    .doc_id
                    .clone()
                    .context("accepted output write has no physical tool ID")?;
                calls_by_doc.entry(doc_id).or_default().push(call);
            }
            for row in rows {
                let matches = calls_by_doc
                    .get(&row.doc_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                anyhow::ensure!(
                    matches.len() == 1,
                    "completed output write has no unique physical tool binding"
                );
                let call = &matches[0];
                anyhow::ensure!(
                    call.session_id == row.session_id
                        && call.request_doc_id.as_deref() == Some(row.request_doc_id.as_str())
                        && call.tool_name == row.tool_name
                        && call.lifecycle_state.as_deref() == Some("completed"),
                    "completed output write changed its accepted identity or terminal state"
                );
                writes
                    .entry(row.tool_name.clone())
                    .or_default()
                    .push(CompletedWriteRow {
                        args: call.args.clone(),
                    });
            }
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
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<UnmetOutputObligation>,
                        gents_loop::output_obligation::OutputObligationCheckError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            OutputObligationGate::unmet(self).await.map_err(|error| {
                gents_loop::output_obligation::OutputObligationCheckError(error.into())
            })
        })
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
#[path = "output_obligation/logical_tests.rs"]
mod logical_tests;
