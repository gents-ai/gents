use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use super::super::projection::ProjectionEngine;
use super::sender::PromptSender;
use super::wire::{
    prompt_text, stop_reason_from_rows, stop_reason_from_wire, PromptRequest, StopReason,
    TerminalOutcome,
};
use super::{
    register_transient_read_retry, transient_graphql_read_retry_delay, wait_for_projected_change,
    CancelBeforeIdLatch, ObservedRequest, TurnManager,
};

impl TurnManager {
    pub(super) async fn submit_request(
        &self,
        request: &PromptRequest,
        _prompt_id: &str,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let content = prompt_text(request);
        let graphql = gents::store::config_client::GraphqlEndpoint::as_principal(
            self.config.graphql.clone(),
            self.config.actor.to_string(),
        );
        let stable_request_id = uuid::Uuid::new_v4().to_string();
        let options = crate::RequestSubmitOptions::default();
        let submitted = if let Some(super::super::goals::GoalCommand::Create {
            objective,
            token_budget,
        }) = super::super::goals::GoalCommand::from_prompt(request)?
        {
            crate::create_goal_backed_agent_request_local(
                &self.node,
                self.config.actor.clone(),
                &graphql,
                &self.config.node_did,
                &objective,
                token_budget,
                &request.session_id,
                &self.config.agent_id,
                stable_request_id.clone(),
                options,
            )
            .await
        } else {
            // The shim owns the destination node, so it binds the canonical
            // Goal directly; the signed remote observation exists for callers
            // that cannot read it, and lags a session the shim just opened.
            let mut options = options;
            let (node_did, agent_id) = (&*self.config.node_did, &*self.config.agent_id);
            let session_id = &*request.session_id;
            options.input = Some(
                gents::ConfigAccess::local(self.node.clone())
                    .transact_readonly("grok.prompt.goal_input", |txn| {
                        Box::pin(gents::goal::prepare_goal_input_in_txn(
                            txn,
                            node_did,
                            session_id,
                            agent_id,
                            Some(node_did),
                            Default::default(),
                        ))
                    })
                    .await?,
            );
            let prepared = crate::request_helpers::prepare_agent_request_with_input(
                &graphql,
                &self.config.node_did,
                &content,
                Some(&request.session_id),
                Some(&self.config.agent_id),
                Some(stable_request_id.clone()),
                options,
            )
            .await?;
            let result = crate::request_helpers::submit_prepared_agent_request_committed(
                &graphql, &prepared,
            )
            .await;
            if result.is_err() {
                if let Ok(Some(row)) =
                    crate::request_helpers::matching_prepared_receipt(&graphql, &prepared.create)
                        .await
                {
                    self.interrupt_submitted(&row).await;
                }
            }
            result
        };
        let submitted = submitted?;
        let row = self
            .load_projection_request(
                &submitted.node_did,
                &submitted.session_id,
                submitted.requester_did.as_deref(),
                &submitted.request_id,
                Some(&submitted.request_doc_id),
            )
            .await;
        if row.is_err() {
            if let Err(error) = gents::interrupt_request_by_doc_id(
                &self.node,
                &submitted.request_doc_id,
                &submitted.node_did,
                submitted.requester_did.as_deref(),
            )
            .await
            {
                tracing::warn!(%error,request_id=%submitted.request_id,"failed to interrupt committed request after projection read failure");
            }
        }
        row
    }

    pub(super) async fn load_projection_request(
        &self,
        agent: &str,
        session: &str,
        requester: Option<&str>,
        request: &str,
        physical: Option<&str>,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let physical_filter = physical
            .map(|id| format!("_docID: {{_eq: \"{}\"}},", escape_graphql_string(id)))
            .unwrap_or_default();
        let query = format!(
            "{{AgentRequest(filter: {{{physical_filter} {}, request_id: {{_eq: \"{}\"}}}},limit:2) {{{}}}}}",
            gents::session::public_request_filter(&gents::session::session_scope_filter(agent, session, requester)),
            escape_graphql_string(request),
            gents::SIGNED_REQUEST_FIELDS
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "load scoped projection request")
                .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .context("projection request query returned no rows")?;
        let [row] = rows.as_slice() else {
            anyhow::bail!("projection request missing or ambiguous");
        };
        let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
        anyhow::ensure!(
            row.request_id == request
                && row.node_did.as_deref() == Some(agent)
                && row.session_id.as_deref() == Some(session)
                && row.requester_did.as_deref() == requester
                && row.doc_id.as_deref().is_some_and(
                    |id| !id.is_empty() && physical.is_none_or(|expected| expected == id)
                ),
            "projection request query crossed exact physical scope"
        );
        Ok(row)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn watch_terminal(
        &self,
        key: &(String, String),
        request: &gents_protocol::row::AgentRequestRow,
        session_id: &str,
        prompt_id: &str,
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        progress: &mut ObservedRequest,
        mut response_rx: oneshot::Receiver<Result<Value>>,
        mut changes: Option<events::DocumentChangeSubscription>,
    ) -> Result<TerminalOutcome> {
        let request_id = request.request_id.as_str();
        let ObservedRequest {
            cursor,
            timing: update_timing,
            ..
        } = progress;
        let mut consecutive_transient_read_failures = 0usize;
        loop {
            if let Ok(result) = response_rx.try_recv() {
                let value = result?;
                let stop_reason = value
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or(StopReason::Cancelled.wire_name());
                return Ok(stop_reason_from_wire(stop_reason).into());
            }
            let projected = self
                .pinned_projection_request(cursor, session_id, request_id, false)
                .await?;
            let terminal = match self.request_terminal(&projected).await {
                Ok(terminal) => terminal,
                Err(error)
                    if register_transient_read_retry(
                        &error,
                        &mut consecutive_transient_read_failures,
                    ) =>
                {
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        retry = consecutive_transient_read_failures,
                        "retrying transient Grok shim terminal query failure"
                    );
                    if let Some(stop_reason) = wait_for_retry_or_cancel(
                        &mut response_rx,
                        transient_graphql_read_retry_delay(consecutive_transient_read_failures),
                    )
                    .await?
                    {
                        return Ok(stop_reason.into());
                    }
                    continue;
                }
                Err(error) => {
                    self.interrupt_and_drain(key, request, generation).await;
                    drop(response_rx);
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        "Grok shim terminal query failed; interrupted submitted request"
                    );
                    return Err(error);
                }
            };
            match self
                .stream_projection_updates(
                    session_id,
                    request_id,
                    Some(prompt_id),
                    sender,
                    projections,
                    cursor,
                    update_timing,
                    false,
                    0,
                )
                .await
            {
                Ok(()) => {}
                Err(error)
                    if register_transient_read_retry(
                        &error,
                        &mut consecutive_transient_read_failures,
                    ) =>
                {
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        retry = consecutive_transient_read_failures,
                        "retrying transient Grok shim projection read failure"
                    );
                    if let Some(stop_reason) = wait_for_retry_or_cancel(
                        &mut response_rx,
                        transient_graphql_read_retry_delay(consecutive_transient_read_failures),
                    )
                    .await?
                    {
                        return Ok(stop_reason.into());
                    }
                    continue;
                }
                Err(error) => {
                    self.interrupt_and_drain(key, request, generation).await;
                    drop(response_rx);
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        "Grok shim live projection failed; interrupted submitted request"
                    );
                    return Err(error);
                }
            }
            consecutive_transient_read_failures = 0;
            if let Some(outcome) = terminal {
                self.take_entry_if_generation(key, generation).await;
                return Ok(outcome);
            }
            tokio::select! {
                () = wait_for_projected_change(&mut changes) => {}
                result = &mut response_rx => {
                    if let Ok(result) = result {
                        let value = result?;
                        let stop_reason = value
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .unwrap_or(StopReason::Cancelled.wire_name());
                        return Ok(stop_reason_from_wire(stop_reason).into());
                    }
                    return Ok(StopReason::Cancelled.into());
                }
            }
        }
    }

    pub(super) async fn request_stop_reason(
        &self,
        request: &gents_protocol::row::AgentRequestRow,
    ) -> Result<Option<StopReason>> {
        Ok(self
            .request_terminal(request)
            .await?
            .map(|outcome| outcome.stop_reason))
    }

    async fn request_terminal(
        &self,
        request: &gents_protocol::row::AgentRequestRow,
    ) -> Result<Option<TerminalOutcome>> {
        let physical = request
            .doc_id
            .as_deref()
            .context("terminal projection physical request missing")?;
        let scope = gents::session::session_scope_filter(
            request
                .node_did
                .as_deref()
                .context("terminal projection owner missing")?,
            request
                .session_id
                .as_deref()
                .context("terminal projection session missing")?,
            request.requester_did.as_deref(),
        );
        let physical = escape_graphql_string(physical);
        let query = format!(
            r#"{{
            AgentRequest(filter: {{{scope}, _docID: {{_eq:"{physical}"}}}},limit:2)
                {{request_id lifecycle_state interrupt_requested_at failure_reason}}
        }}"#
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "grok shim turn terminal query")
                .await?;
        let request_rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .context("terminal projection request rows missing")?;
        anyhow::ensure!(
            request_rows.len() <= 1,
            "ambiguous terminal projection physical scope"
        );
        let Some(request_row) = request_rows.first() else {
            return Ok(None);
        };
        let lifecycle_state = request_row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let interrupted_at = request_row
            .get("interrupt_requested_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let error_message = request_row
            .get("failure_reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok(
            stop_reason_from_rows(lifecycle_state, interrupted_at.as_deref()).map(|stop_reason| {
                TerminalOutcome {
                    stop_reason,
                    error_message,
                }
            }),
        )
    }
}

async fn wait_for_retry_or_cancel(
    response_rx: &mut oneshot::Receiver<Result<Value>>,
    delay: Duration,
) -> Result<Option<StopReason>> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(None),
        result = response_rx => {
            let value = match result {
                Ok(result) => result?,
                Err(_) => return Ok(Some(StopReason::Cancelled)),
            };
            let stop_reason = value
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or(StopReason::Cancelled.wire_name());
            Ok(Some(stop_reason_from_wire(stop_reason)))
        }
    }
}
