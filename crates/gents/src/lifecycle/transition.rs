// Soft-cap justified: single impl block on RequestLifecycle; all methods are
// atomic DB mutations that must stay together to preserve the Lean-spec
// transition invariants (S1, S3, S6). Splitting by transition direction
// (complete/fail/supersede) would require re-exporting private helpers across
// submodules with no readability gain.
use anyhow::Context;
use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::rows::RequestStatusTransition;
use super::*;
use gents_protocol::row::AgentRequestRow;

fn request_view_is_terminal(view: &AgentRequestRow) -> bool {
    view.lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}

pub(super) fn projection_error_requires_atomic_retry(error: &anyhow::Error) -> bool {
    crate::retry::is_defradb_transaction_conflict_text(&error.to_string())
}

async fn execute_request_only_transaction(
    node: &EmbeddedNode,
    request_mutation: &str,
) -> Result<defra_node::QueryResponse> {
    let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
    match txn.execute_local_response(request_mutation).await {
        Ok(response) => txn.commit().await.map(|()| response),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

pub(super) async fn execute_request_projection_transaction(
    node: &EmbeddedNode,
    request_mutation: &str,
    conversation_mutation: &str,
    operation: &str,
) -> Result<defra_node::QueryResponse> {
    crate::retry::retry_terminal_persistence_operation(
        operation,
        crate::retry::TERMINAL_PERSISTENCE_MAX_RETRIES,
        std::time::Duration::from_millis(crate::retry::TERMINAL_PERSISTENCE_INITIAL_BACKOFF_MS),
        || async {
            let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
            let request_response = match txn.execute_local_response(request_mutation).await {
                Ok(response) => response,
                Err(error) => {
                    let _ = txn.discard().await;
                    return Err(error);
                }
            };
            // The request mutation may be an idempotent no-op because the
            // stream writer already committed its terminal edge. The guarded
            // projection update must still run so that retry repairs a stale
            // conversation. A transaction conflict must retry both writes as
            // one atomic attempt. Only deterministic projection errors fall
            // back to the authoritative request in a fresh transaction. Never
            // trust commit-after-error behavior from the storage engine.
            if let Err(error) = txn.execute_local_response(conversation_mutation).await {
                let _ = txn.discard().await;
                if projection_error_requires_atomic_retry(&error) {
                    tracing::warn!(
                        operation,
                        error = %error,
                        "retrying terminal request with its conversation projection"
                    );
                    return Err(error);
                }
                tracing::warn!(
                    operation,
                    error = %error,
                    "committing terminal request without its unavailable conversation projection"
                );
                return execute_request_only_transaction(node, request_mutation).await;
            }
            txn.commit().await.map(|()| request_response)
        },
    )
    .await
}

impl RequestLifecycle {
    pub(crate) async fn ensure_error_response(&mut self, reason: &str) -> Result<()> {
        let request_id = escape_graphql_string(&self.request.request_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let query = format!(
            r#"{{
                AgentResponse(
                    filter: {{
                        request_id: {{ _eq: "{request_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }}
                    }},
                    limit: 1
                ) {{ _docID status }}
            }}"#
        );
        let existing = self.node.execute(&query).await;
        if existing.has_errors() {
            anyhow::bail!(
                "querying error response for request {}: {:?}",
                self.request.request_id,
                existing.errors
            );
        }
        if let Some(row) = existing
            .data
            .as_ref()
            .and_then(|data| data.get("AgentResponse"))
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
        {
            let Some(doc_id) = row.get("_docID").and_then(serde_json::Value::as_str) else {
                anyhow::bail!(
                    "existing response for request {} has no document id",
                    self.request.request_id
                );
            };
            self.response_doc_id = Some(doc_id.to_string());
            if row.get("status").and_then(serde_json::Value::as_str) == Some("complete") {
                return Ok(());
            }
            let doc_id = escape_graphql_string(doc_id);
            let content = escape_graphql_string(&format!("Error: {reason}"));
            let reason = escape_graphql_string(reason);
            let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
            let mutation = format!(
                r#"mutation {{
                    update_AgentResponse(
                        filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                        input: {{
                            content: "{content}",
                            reasoning: "",
                            status: "error",
                            error_message: "{reason}",
                            completed_at: "{now}"
                        }}
                    ) {{ _docID }}
                }}"#
            );
            crate::retry::execute_graphql_with_terminal_persistence_retry(
                &self.node,
                &mutation,
                "terminalize_existing_request_error_response",
            )
            .await?;
            return Ok(());
        }

        let request_doc_id = escape_graphql_string(&self.request.doc_id);
        let behavior_id = escape_graphql_string(&self.behavior_id);
        let session_id = escape_graphql_string(&self.request.session_id);
        let content = escape_graphql_string(&format!("Error: {reason}"));
        let reason = escape_graphql_string(reason);
        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let requester_did_field =
            session::requester_did_create_field(self.request.requester_did.as_deref());
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{request_id}",
                    request_id: "{request_id}",
                    request_doc_id: "{request_doc_id}",
                    agent_did: "{agent_did}",
                    {requester_did_field}
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    content: "{content}",
                    reasoning: "",
                    status: "error",
                    error_message: "{reason}",
                    token_count: 0,
                    progress_seq: 0,
                    created_at: "{now}",
                    completed_at: "{now}"
                }}) {{ _docID }}
            }}"#
        );
        let response = crate::retry::execute_graphql_with_terminal_persistence_retry(
            &self.node,
            &mutation,
            "create_request_error_response",
        )
        .await?;
        self.response_doc_id = extract_single_doc_id(&response, "create_AgentResponse");
        if self.response_doc_id.is_none() {
            anyhow::bail!(
                "creating error response for request {} returned no document",
                self.request.request_id
            );
        }
        Ok(())
    }

    pub async fn record_failure_reason(&mut self, reason: &str) -> Result<()> {
        // Latch before I/O so the subsequent atomic terminal mutation still
        // carries the reason if this best-effort standalone write fails.
        self.failure_reason = Some(reason.to_string());
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let escaped_reason = escape_graphql_string(reason);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }}
                    }},
                    input: {{ failure_reason: "{escaped_reason}" }}
                ) {{ _docID }}
            }}"#
        );

        crate::retry::execute_graphql_with_terminal_persistence_retry(
            &self.node,
            &mutation,
            "record_request_failure_reason",
        )
        .await
        .with_context(|| {
            format!(
                "recording failure reason for request {} doc_id={doc_id}",
                self.request.request_id
            )
        })?;
        Ok(())
    }

    pub async fn advance(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Streaming], "advance")?;
        let doc_id = self
            .response_doc_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("advance() called before response doc created"))?;
        let next_progress_seq = self.progress_seq + 1;
        tracing::debug!(
            request_id = %self.request.request_id,
            doc_id = %doc_id,
            next_progress_seq,
            "advancing response progress"
        );

        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{ progress_seq: {progress_seq} }}
                ) {{ _docID }}
            }}"#,
            progress_seq = next_progress_seq,
        );

        let operation = format!("advance_progress_seq_{next_progress_seq}");
        let resp = session::execute_mutation_with_retry(&self.node, &mutation, &operation)
            .await
            .with_context(|| {
                format!(
                    "failed to advance progress_seq for doc_id={doc_id} progress_seq={next_progress_seq}"
                )
            })?;

        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentResponse"))
            .is_some_and(response_has_documents)
        {
            let status = self.response_status().await?;
            anyhow::bail!(
                "cannot advance progress for request_id={} doc_id={doc_id}: response is {}",
                self.request.request_id,
                status.as_deref().unwrap_or("missing")
            );
        }

        self.progress_seq = next_progress_seq;
        Ok(())
    }

    pub async fn complete(&mut self) -> Result<()> {
        if self.state == LocalLifecycleState::Completed {
            return Ok(());
        }
        if self.state == LocalLifecycleState::Failed {
            tracing::info!(
                request_id = %self.request.request_id,
                "skipping complete() because request lifecycle already failed"
            );
            return Ok(());
        }
        self.ensure_state(
            &[LocalLifecycleState::Claimed, LocalLifecycleState::Streaming],
            "complete",
        )?;

        match self
            .transition_request_status(
                &[
                    RequestLifecycleState::Claimed,
                    RequestLifecycleState::Processing,
                ],
                RequestLifecycleState::Completed,
                "completed",
            )
            .await?
        {
            RequestStatusTransition::Updated | RequestStatusTransition::AlreadyTarget => {}
            RequestStatusTransition::ConflictingTerminal(current) => {
                tracing::info!(
                    request_id = %self.request.request_id,
                    current_lifecycle_state = %current.lifecycle_state.map(|s| s.as_str()).unwrap_or("missing"),
                    "skipping completion because request is already terminal"
                );
            }
        }

        self.state = LocalLifecycleState::Completed;
        tracing::info!(
            request_id = %self.request.request_id,
            session_id = %self.request.session_id,
            "request completed"
        );
        Ok(())
    }

    pub async fn transition_to_interrupted(&mut self) -> Result<()> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        let terminalized_at_value = chrono::Utc::now().to_rfc3339();
        let terminalized_at = escape_graphql_string(&terminalized_at_value);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _in: {active_runtime_states} }}
                    }},
                    input: {{
                        lifecycle_state: "interrupted",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let conversation_mutation = session::request_conversation_status_projection_mutation(
            &self.request.session_id,
            &self.request.request_id,
            "active",
            &terminalized_at_value,
        );
        let resp = execute_request_projection_transaction(
            &self.node,
            &mutation,
            &conversation_mutation,
            "transition_interrupted",
        )
        .await?;
        if resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            self.state = LocalLifecycleState::Interrupted;
            return Ok(());
        }

        match self.request_view().await? {
            Some(current)
                if current.lifecycle_state == Some(RequestLifecycleState::Interrupted) =>
            {
                self.state = LocalLifecycleState::Interrupted;
                Ok(())
            }
            Some(current) if request_view_is_terminal(&current) => Ok(()),
            Some(current)
                if current.lifecycle_state == Some(RequestLifecycleState::InputRequired) =>
            {
                anyhow::bail!(
                    "cannot interrupt request_id={} from reserved lifecycle_state={}",
                    self.request.request_id,
                    RequestLifecycleState::InputRequired.as_str()
                )
            }
            Some(current) => anyhow::bail!(
                "request {} could not transition to interrupted; current lifecycle_state={}",
                self.request.request_id,
                current
                    .lifecycle_state
                    .map(|s| s.as_str())
                    .unwrap_or("missing")
            ),
            None => anyhow::bail!(
                "request {} disappeared while transitioning to interrupted",
                self.request.request_id
            ),
        }
    }

    pub async fn fail(&mut self) -> Result<()> {
        if self.state == LocalLifecycleState::Failed {
            return Ok(());
        }
        if self.state == LocalLifecycleState::Completed {
            tracing::info!(
                request_id = %self.request.request_id,
                "skipping fail() because request lifecycle already completed"
            );
            return Ok(());
        }
        if crate::interrupt::fetch_interrupt_requested_at(&self.node, &self.request.request_id)
            .await?
            .is_some()
        {
            tracing::info!(
                request_id = %self.request.request_id,
                "request failure observed after interrupt_requested_at was latched; transitioning to interrupted"
            );
            self.transition_to_interrupted().await?;
            return Ok(());
        }
        self.ensure_state(
            &[LocalLifecycleState::Claimed, LocalLifecycleState::Streaming],
            "fail",
        )?;

        match self
            .transition_request_status(
                &[
                    RequestLifecycleState::Claimed,
                    RequestLifecycleState::Processing,
                ],
                RequestLifecycleState::Failed,
                "active",
            )
            .await?
        {
            RequestStatusTransition::Updated | RequestStatusTransition::AlreadyTarget => {}
            RequestStatusTransition::ConflictingTerminal(current) => {
                tracing::info!(
                    request_id = %self.request.request_id,
                    current_lifecycle_state = %current.lifecycle_state.map(|s| s.as_str()).unwrap_or("missing"),
                    "skipping failure because request is already terminal"
                );
            }
        }

        self.state = LocalLifecycleState::Failed;
        tracing::info!(
            request_id = %self.request.request_id,
            session_id = %self.request.session_id,
            "request failed"
        );
        Ok(())
    }

    /// Atomically persist the failure reason with the request's terminal edge.
    /// The reason is latched in memory before any storage attempt, so callers do
    /// not depend on a separate `failure_reason` mutation succeeding first.
    pub async fn fail_with_reason(&mut self, reason: &str) -> Result<()> {
        self.failure_reason = Some(reason.to_string());
        self.fail().await
    }

    pub(super) async fn transition_request_status(
        &self,
        from_lifecycle_states: &[RequestLifecycleState],
        target_lifecycle_state: RequestLifecycleState,
        conversation_status: &str,
    ) -> Result<RequestStatusTransition> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let from_lifecycle_states_list =
            RequestLifecycleState::graphql_list(from_lifecycle_states.iter().copied());
        let terminalized_at_value = chrono::Utc::now().to_rfc3339();
        let terminalized_at = escape_graphql_string(&terminalized_at_value);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _in: {from_lifecycle_states_list} }}
                    }},
                    input: {{
                        lifecycle_state: "{target_lifecycle_state}",
                        backend_id: "{backend_id}",
                        failure_reason: "{failure_reason}",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#,
            target_lifecycle_state = target_lifecycle_state.as_str(),
            backend_id = escape_graphql_string(&self.backend_id),
            failure_reason =
                escape_graphql_string(self.failure_reason.as_deref().unwrap_or_default()),
        );

        let conversation_mutation = session::request_conversation_status_projection_mutation(
            &self.request.session_id,
            &self.request.request_id,
            conversation_status,
            &terminalized_at_value,
        );
        let resp = execute_request_projection_transaction(
            &self.node,
            &mutation,
            &conversation_mutation,
            "transition_request_terminal_status",
        )
        .await
        .with_context(|| {
            format!(
                "updating request lifecycle_state -> {} for doc_id={doc_id}",
                target_lifecycle_state.as_str()
            )
        })?;

        if resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            tracing::debug!(
                doc_id = %doc_id,
                target_lifecycle_state = %target_lifecycle_state.as_str(),
                "updated request lifecycle_state"
            );
            return Ok(RequestStatusTransition::Updated);
        }

        match self.request_view().await? {
            Some(current) if current.lifecycle_state == Some(target_lifecycle_state) => {
                Ok(RequestStatusTransition::AlreadyTarget)
            }
            Some(current) if request_view_is_terminal(&current) => {
                Ok(RequestStatusTransition::ConflictingTerminal(current))
            }
            Some(current) => {
                anyhow::bail!(
                "request {} could not transition lifecycle_state -> {}; current lifecycle_state={}",
                self.request.request_id,
                target_lifecycle_state.as_str(),
                current.lifecycle_state.map(|s| s.as_str()).unwrap_or("missing")
            )
            }
            None => anyhow::bail!(
                "request {} disappeared while transitioning lifecycle_state -> {}",
                self.request.request_id,
                target_lifecycle_state.as_str()
            ),
        }
    }

    pub(super) async fn transition_execution_view(
        &self,
        from: RequestLifecycleState,
        to: RequestLifecycleState,
    ) -> Result<()> {
        let doc_id = self.request.doc_id.clone();
        let escaped_doc_id = escape_graphql_string(&doc_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "{from}" }}
                    }},
                    input: {{
                        lifecycle_state: "{to}",
                        backend_id: "{backend_id}",
                        failure_reason: "{failure_reason}"
                    }}
                ) {{ _docID }}
            }}"#,
            backend_id = escape_graphql_string(&self.backend_id),
            failure_reason =
                escape_graphql_string(self.failure_reason.as_deref().unwrap_or_default()),
        );

        let operation = format!("transition_execution_view_{from}_to_{to}");
        let resp = session::execute_mutation_with_retry(&self.node, &mutation, &operation)
            .await
            .with_context(|| {
                format!("updating execution view {from} -> {to} for doc_id={doc_id}")
            })?;

        if resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            return Ok(());
        }

        let request_view = self.request_view().await?;
        match request_view {
            Some(current) if current.lifecycle_state == Some(to) => Ok(()),
            Some(current) => anyhow::bail!(
                "request {} could not transition execution view {} -> {}; current lifecycle_state={}",
                self.request.request_id,
                from,
                to,
                current.lifecycle_state.map(|s| s.as_str()).unwrap_or("missing")
            ),
            None => anyhow::bail!(
                "request {} disappeared while transitioning execution view {} -> {}",
                self.request.request_id,
                from,
                to
            ),
        }
    }

    pub(super) fn ensure_state(
        &self,
        expected: &[LocalLifecycleState],
        action: &str,
    ) -> Result<()> {
        if expected.contains(&self.state) {
            return Ok(());
        }

        anyhow::bail!(
            "cannot {} request_id={} while lifecycle is in {:?}",
            action,
            self.request.request_id,
            self.state
        )
    }
}
