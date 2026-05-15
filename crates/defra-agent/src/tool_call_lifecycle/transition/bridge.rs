use super::*;

impl ToolCallLifecycle {
    /// Running → Completed for bridge (subagent) tools.
    ///
    /// Lean parity: bridge_complete. Parent tool .running → .completed when
    /// the caller has verified the linked child request reached .completed.
    /// Persists child_result as the row's `result` field; sets state,
    /// completed_at, latency_ms following R1's complete() persistence pattern.
    ///
    /// Trust boundary: bridge_complete does NOT verify the child's terminal
    /// state internally (Lean's precondition is on the caller). R3's
    /// SubagentSource will be the natural place for that check.
    pub async fn bridge_complete(&mut self, child_result: String) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "bridge_complete")?;
        if !self.is_bridge() {
            return Err(IllegalToolCallTransition::BridgeCompleteRequiresChildLink.into());
        }

        let doc_id = self.doc_id.as_ref().ok_or_else(|| {
            anyhow!("bridge_complete called before start_running persisted a row")
        })?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("bridge_complete called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_result = escape_graphql_string(&child_result);
        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update to
        // avoid a type-mismatch error when re-validating the document.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }}
                    }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "completed",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                        {unclaimed_deadline_clear}
                    }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "bridge_complete")
            .await
            .context("bridge_complete mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_running_compare("bridge_complete")
                .await?;
            return Ok(false);
        }

        self.state = ToolCallState::Completed;
        Ok(true)
    }

    /// Running → Failed (or Cancelled for ChildTerminal::Interrupted).
    ///
    /// Lean parity: bridge_failure. Parent tool .running → .failed (or
    /// .cancelled for ChildTerminal::Interrupted). Projection per
    /// ChildTerminal::projected_state(). Persists lifecycle_state,
    /// completed_at, latency_ms; conditionally persists tool_failure_class
    /// and result when the child reached .failed.
    ///
    /// Returns BridgeFailureRequiresChildLink for native tools (no
    /// child_request_id).
    pub async fn bridge_failure(&mut self, child_terminal: super::ChildTerminal) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "bridge_failure")?;
        if !self.is_bridge() {
            return Err(IllegalToolCallTransition::BridgeFailureRequiresChildLink.into());
        }

        let projected = child_terminal.projected_state();
        let (failure_class_for_persist, reason_for_persist) = match &child_terminal {
            super::ChildTerminal::Failed {
                reason,
                failure_class,
            } => (Some(*failure_class), Some(reason.clone())),
            _ => (None, None),
        };

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("bridge_failure called before start_running persisted a row"))?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("bridge_failure called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let lifecycle_state_str = projected.as_str();
        let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();

        // Build conditional fields: tool_failure_class and result are only
        // set when the child reached .failed (mirrors R1's fail() pattern).
        let optional_fields = match (failure_class_for_persist, reason_for_persist.as_deref()) {
            (Some(fc), Some(reason)) => {
                let escaped_reason = escape_graphql_string(reason);
                let fc_str = fc.as_str();
                format!(
                    r#"result: "{escaped_reason}",
                        tool_failure_class: "{fc_str}","#
                )
            }
            _ => String::new(),
        };

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }}
                    }},
                    input: {{
                        {optional_fields}
                        status: "completed",
                        lifecycle_state: "{lifecycle_state_str}",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                        {unclaimed_deadline_clear}
                    }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "bridge_failure")
            .await
            .context("bridge_failure mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_running_compare("bridge_failure")
                .await?;
            return Ok(false);
        }

        self.state = projected;
        self.failure_class = failure_class_for_persist;
        Ok(true)
    }

    /// Lean parity: bridge_cancel_cascade. Pure — returns the action that should
    /// be taken on the child AgentRequest. Caller (typically R3's daemon
    /// interrupt dispatcher) performs the actual write to set
    /// interrupt_requested_at on the child. Returns None for native tools,
    /// detached subagents, or non-cancelled bridge tools.
    pub async fn bridge_cancel_cascade(&self) -> Result<Option<super::CascadeIntent>> {
        if self.state != ToolCallState::Cancelled {
            return Err(IllegalToolCallTransition::CascadeRequiresCancelled.into());
        }
        if self.cancel_policy != CancelPolicy::Cascade {
            return Ok(None); // detached: no cascade
        }
        let Some(child_request_id) = self.child_request_id.clone() else {
            return Ok(None); // native: no bridge edge
        };
        Ok(Some(super::CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        }))
    }

    /// Dispatch cascade cancellation according to child ownership. Local child
    /// requests continue through the existing interrupt path; replicated
    /// cross-deployment children are signaled through the bridge row.
    pub async fn bridge_cancel_cascade_dispatch(
        &self,
        local_did: &str,
    ) -> Result<Option<CascadeDispatch>> {
        let Some(intent) = self.bridge_cancel_cascade().await? else {
            return Ok(None);
        };

        if child_request_is_locally_owned(&self.node, local_did, &intent.child_request_id).await? {
            return Ok(Some(CascadeDispatch::Local(intent)));
        }

        self.write_bridge_cancel_cascade_intent(intent.at).await?;
        Ok(Some(CascadeDispatch::RemoteIntentWritten))
    }

    async fn write_bridge_cancel_cascade_intent(
        &self,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let doc_id = self.doc_id.as_ref().ok_or_else(|| {
            anyhow!("bridge_cancel_cascade_dispatch called before row was persisted")
        })?;
        let escaped_doc_id = escape_graphql_string(doc_id);
        let at = escape_graphql_string(&at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let started_at = self.started_at.ok_or_else(|| {
            anyhow!("bridge_cancel_cascade_dispatch called without started_at set")
        })?;
        let started_at = started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let deadline_at = self
            .deadline_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        started_at: "{started_at}",
                        deadline_at: "{deadline_at}",
                        completed_at: "{at}",
                        cancel_cascade_intent_at: "{at}",
                        cancel_pending_remote_ack: true
                        {unclaimed_deadline_clear}
                    }}
                ) {{ _docID }}
            }}"#
        );
        execute_mutation_with_retry(&self.node, &mutation, "write_bridge_cancel_cascade_intent")
            .await
            .context("write bridge cancel cascade intent mutation")?;
        Ok(())
    }

    /// Running → Cancelled. Called by request interruption handling and
    /// startup recovery for interrupted parent requests.
    ///
    pub async fn cancel_during_run(&mut self) -> Result<()> {
        self.cancel_during_run_inner(None).await
    }

    /// Running -> Cancelled while dispatching a cascade cancel. For remote
    /// children the durable cancel intent is written in the same bridge update
    /// that terminalizes the tool call, so recovery never observes a cancelled
    /// bridge without the remote signal.
    pub async fn cancel_during_run_with_cascade_dispatch(
        &mut self,
        local_did: &str,
    ) -> Result<Option<CascadeDispatch>> {
        self.ensure_state(
            &[ToolCallState::Running],
            "cancel_during_run_with_cascade_dispatch",
        )?;

        let Some(child_request_id) = self.child_request_id.clone() else {
            self.cancel_during_run_inner(None).await?;
            return Ok(None);
        };
        if self.cancel_policy != CancelPolicy::Cascade {
            self.cancel_during_run_inner(None).await?;
            return Ok(None);
        }

        let intent = super::CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        };
        if child_request_is_locally_owned(&self.node, local_did, &intent.child_request_id).await? {
            self.cancel_during_run_inner(None).await?;
            if self.is_cancelled() {
                return Ok(Some(CascadeDispatch::Local(intent)));
            }
            return Ok(None);
        }

        self.cancel_during_run_inner(Some(intent.at)).await?;
        if self.is_cancelled() {
            Ok(Some(CascadeDispatch::RemoteIntentWritten))
        } else {
            Ok(None)
        }
    }

    async fn cancel_during_run_inner(
        &mut self,
        remote_cancel_intent_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "cancel_during_run")?;

        let doc_id = self.doc_id.as_ref().ok_or_else(|| {
            anyhow!("cancel_during_run called before start_running persisted a row")
        })?;
        let now = Utc::now();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("cancel_during_run called without started_at set"))?;
        let latency_ms = (now - started_at).num_milliseconds();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let escaped_result = escape_graphql_string("tool call cancelled");
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let started_at_str = started_at.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();
        let remote_cancel_intent_fragment = remote_cancel_intent_at
            .map(|at| {
                let at = escape_graphql_string(&at.to_rfc3339());
                format!(
                    r#",
                        cancel_cascade_intent_at: "{at}",
                        cancel_pending_remote_ack: true"#
                )
            })
            .unwrap_or_default();

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "running" }}
                    }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "cancelled",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: {latency_ms}
                        {remote_cancel_intent_fragment}
                        {unclaimed_deadline_clear}
                    }}
                ) {{ _docID }}
            }}"#
        );

        let response = execute_mutation_with_retry(&self.node, &mutation, "cancel_during_run")
            .await
            .context("cancel_during_run mutation")?;
        if !response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentToolCall"))
            .is_some_and(response_has_documents)
        {
            self.sync_after_lost_running_compare("cancel_during_run")
                .await?;
            return Ok(());
        }

        self.state = ToolCallState::Cancelled;
        Ok(())
    }
}

async fn child_request_is_locally_owned(
    node: &defra_node::EmbeddedNode,
    local_did: &str,
    child_request_id: &str,
) -> Result<bool> {
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ agent_did }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest for cross-deployment cancel dispatch failed: {:?}",
            response.errors
        );
    }
    let did = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("agent_did"))
        .and_then(|v| v.as_str());
    Ok(did == Some(local_did))
}
