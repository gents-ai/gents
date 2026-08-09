use super::*;

enum BridgeTerminalEvidence {
    Output(crate::SignedDocumentVersionRef),
    Omission(crate::SignedDocumentVersionRef),
}

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
        let Some(mut output) = self
            .retain_terminal_output_fact_or_adopt(
                ToolCallState::Running,
                "bridge_complete",
                &child_result,
            )
            .await?
        else {
            return Ok(false);
        };

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("bridge_complete called before start_running persisted a row"))?
            .clone();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("bridge_complete called without started_at set"))?;
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let output_fields = exact_result_fields_fragment(&output);
            let mutation = format!(
                r#"mutation {{
                    update_AgentToolCall(
                        filter: {{ _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }} }},
                        input: {{
                            result: "{}",
                            status: "{}",
                            lifecycle_state: "completed",
                            started_at: "{}",
                            deadline_at: "{}",
                            completed_at: "{}",
                            {output_fields}
                            latency_ms: {}
                            {}
                        }}
                    ) {{ _docID }}
                }}"#,
                escape_graphql_string(&doc_id),
                escape_graphql_string(&child_result),
                self.terminal_persistence_status(None),
                started_at.to_rfc3339(),
                self.deadline_at.to_rfc3339(),
                now.to_rfc3339(),
                (now - started_at).num_milliseconds(),
                self.clear_unclaimed_deadline_fragment(),
            );
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Running,
                &[ExactToolEvidence {
                    collection: "AgentToolResult",
                    exact: &output,
                    require_execution_owner: true,
                }],
                &mutation,
                "update_AgentToolCall",
                "bridge_complete_with_exact_output",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::Completed;
                    return Ok(true);
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_running_compare("bridge_complete")
                        .await?;
                    return Ok(false);
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(next_output) = self
                        .retain_terminal_output_fact_or_adopt(
                            ToolCallState::Running,
                            "bridge_complete",
                            &child_result,
                        )
                        .await?
                    else {
                        return Ok(false);
                    };
                    output = next_output;
                }
                ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                    "AgentToolCall {doc_id} kept changing while binding bridge output"
                ),
            }
        }
        unreachable!("bounded exact-output loop returns on every outcome")
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
        let completion_reason = match &child_terminal {
            super::ChildTerminal::Dead => "deadline_exceeded",
            super::ChildTerminal::Interrupted => "explicit_cancel",
            super::ChildTerminal::Failed { .. } | super::ChildTerminal::Superseded => "tool_failed",
        };
        self.bridge_failure_with_completion_reason(child_terminal, completion_reason)
            .await
    }

    pub(crate) async fn bridge_failure_with_completion_reason(
        &mut self,
        child_terminal: super::ChildTerminal,
        completion_reason: &str,
    ) -> Result<bool> {
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
            .ok_or_else(|| anyhow!("bridge_failure called before start_running persisted a row"))?
            .clone();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("bridge_failure called without started_at set"))?;
        let (omission_reason, evidence_detail) = match &child_terminal {
            super::ChildTerminal::Failed { reason, .. } if !reason.trim().is_empty() => {
                (None, reason.as_str())
            }
            super::ChildTerminal::Failed { .. } => (
                Some(super::super::evidence::ToolOutputOmissionReason::RecoveryFailure),
                "child request failed without a durable output",
            ),
            super::ChildTerminal::Dead => (
                Some(super::super::evidence::ToolOutputOmissionReason::ChildDead),
                "child request reached the dead terminal state",
            ),
            super::ChildTerminal::Interrupted => (
                Some(super::super::evidence::ToolOutputOmissionReason::Cancelled),
                "child request was interrupted",
            ),
            super::ChildTerminal::Superseded => (
                Some(super::super::evidence::ToolOutputOmissionReason::ChildSuperseded),
                "child request was superseded",
            ),
        };
        let mut evidence = match omission_reason {
            Some(reason) => {
                let Some(omission) = self
                    .retain_terminal_omission_fact_or_adopt(
                        ToolCallState::Running,
                        projected,
                        reason,
                        evidence_detail,
                        "bridge_failure",
                    )
                    .await?
                else {
                    return Ok(false);
                };
                BridgeTerminalEvidence::Omission(omission)
            }
            None => {
                let Some(output) = self
                    .retain_terminal_output_fact_or_adopt(
                        ToolCallState::Running,
                        "bridge_failure",
                        evidence_detail,
                    )
                    .await?
                else {
                    return Ok(false);
                };
                BridgeTerminalEvidence::Output(output)
            }
        };

        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let exact_fields = match &evidence {
                BridgeTerminalEvidence::Output(output) => exact_result_fields_fragment(output),
                BridgeTerminalEvidence::Omission(omission) => {
                    exact_omission_fields_fragment(omission)
                }
            };
            let result_fields = match (failure_class_for_persist, reason_for_persist.as_deref()) {
                (Some(failure), Some(reason)) if !reason.trim().is_empty() => format!(
                    r#"result: "{}", tool_failure_class: "{}","#,
                    escape_graphql_string(reason),
                    failure.as_str(),
                ),
                (Some(failure), _) => {
                    format!(r#"tool_failure_class: "{}","#, failure.as_str())
                }
                _ => String::new(),
            };
            let cancel_cause_field = (projected == ToolCallState::Cancelled)
                .then(|| format!(r#"cancel_cause: "{}","#, CancelCause::Interrupted.as_str()))
                .unwrap_or_default();
            let mutation = format!(
                r#"mutation {{
                    update_AgentToolCall(
                        filter: {{ _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }} }},
                        input: {{
                            {result_fields}
                            {cancel_cause_field}
                            {exact_fields}
                            status: "{}",
                            lifecycle_state: "{}",
                            started_at: "{}",
                            deadline_at: "{}",
                            completed_at: "{}",
                            latency_ms: {}
                            {}
                        }}
                    ) {{ _docID }}
                }}"#,
                escape_graphql_string(&doc_id),
                self.terminal_persistence_status(Some(completion_reason)),
                projected.as_str(),
                started_at.to_rfc3339(),
                self.deadline_at.to_rfc3339(),
                now.to_rfc3339(),
                (now - started_at).num_milliseconds(),
                self.clear_unclaimed_deadline_fragment(),
            );
            let exact = match &evidence {
                BridgeTerminalEvidence::Output(output) => ExactToolEvidence {
                    collection: "AgentToolResult",
                    exact: output,
                    require_execution_owner: true,
                },
                BridgeTerminalEvidence::Omission(omission) => ExactToolEvidence {
                    collection: "AgentToolOutputOmission",
                    exact: omission,
                    require_execution_owner: true,
                },
            };
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Running,
                &[exact],
                &mutation,
                "update_AgentToolCall",
                "bridge_failure_with_exact_evidence",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = projected;
                    self.failure_class = failure_class_for_persist;
                    self.cancel_cause =
                        (projected == ToolCallState::Cancelled).then_some(CancelCause::Interrupted);
                    return Ok(true);
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_running_compare("bridge_failure")
                        .await?;
                    return Ok(false);
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    evidence = match omission_reason {
                        Some(reason) => {
                            let Some(omission) = self
                                .retain_terminal_omission_fact_or_adopt(
                                    ToolCallState::Running,
                                    projected,
                                    reason,
                                    evidence_detail,
                                    "bridge_failure",
                                )
                                .await?
                            else {
                                return Ok(false);
                            };
                            BridgeTerminalEvidence::Omission(omission)
                        }
                        None => {
                            let Some(output) = self
                                .retain_terminal_output_fact_or_adopt(
                                    ToolCallState::Running,
                                    "bridge_failure",
                                    evidence_detail,
                                )
                                .await?
                            else {
                                return Ok(false);
                            };
                            BridgeTerminalEvidence::Output(output)
                        }
                    };
                }
                ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                    "AgentToolCall {doc_id} kept changing while binding bridge terminal evidence"
                ),
            }
        }
        unreachable!("bounded exact-evidence loop returns on every outcome")
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
    pub async fn cancel_during_run(&mut self, cause: CancelCause) -> Result<bool> {
        self.cancel_during_run_inner(cause, None, None).await
    }

    /// Returns whether this caller won the durable running-state compare.
    /// Background completion side effects must only be projected by that
    /// winner; a loser adopts the already-terminal durable row.
    pub(crate) async fn cancel_during_run_owned(
        &mut self,
        cause: CancelCause,
        completion_reason: &str,
    ) -> Result<bool> {
        self.cancel_during_run_inner(cause, None, Some(completion_reason))
            .await
    }

    /// Running -> Cancelled while dispatching a cascade cancel. For remote
    /// children the durable cancel intent is written in the same bridge update
    /// that terminalizes the tool call, so recovery never observes a cancelled
    /// bridge without the remote signal.
    pub async fn cancel_during_run_with_cascade_dispatch(
        &mut self,
        cause: CancelCause,
        local_did: &str,
    ) -> Result<Option<CascadeDispatch>> {
        self.ensure_state(
            &[ToolCallState::Running],
            "cancel_during_run_with_cascade_dispatch",
        )?;

        let Some(child_request_id) = self.child_request_id.clone() else {
            let _ = self.cancel_during_run_inner(cause, None, None).await?;
            return Ok(None);
        };
        if self.cancel_policy != CancelPolicy::Cascade {
            let _ = self.cancel_during_run_inner(cause, None, None).await?;
            return Ok(None);
        }

        let intent = super::CascadeIntent {
            child_request_id,
            at: chrono::Utc::now(),
        };
        if child_request_is_locally_owned(&self.node, local_did, &intent.child_request_id).await? {
            let won = self.cancel_during_run_inner(cause, None, None).await?;
            if won {
                return Ok(Some(CascadeDispatch::Local(intent)));
            }
            return Ok(None);
        }

        let won = self
            .cancel_during_run_inner(cause, Some(intent.at), None)
            .await?;
        if won {
            Ok(Some(CascadeDispatch::RemoteIntentWritten))
        } else {
            Ok(None)
        }
    }

    async fn cancel_during_run_inner(
        &mut self,
        cause: CancelCause,
        remote_cancel_intent_at: Option<chrono::DateTime<chrono::Utc>>,
        completion_reason_override: Option<&str>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "cancel_during_run")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| {
                anyhow!("cancel_during_run called before start_running persisted a row")
            })?
            .clone();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("cancel_during_run called without started_at set"))?;
        let completion_reason = completion_reason_override.unwrap_or(match cause {
            CancelCause::Deadline => "deadline_exceeded",
            CancelCause::Interrupted => "parent_interrupted",
            CancelCause::UserCancelled => "explicit_cancel",
        });
        let detail = "tool call cancelled";
        let Some(mut omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::Running,
                ToolCallState::Cancelled,
                super::super::evidence::ToolOutputOmissionReason::Cancelled,
                detail,
                "cancel_during_run",
            )
            .await?
        else {
            return Ok(false);
        };
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let remote_cancel_intent_fragment = remote_cancel_intent_at
                .map(|at| {
                    format!(
                        r#",
                            cancel_cascade_intent_at: "{}",
                            cancel_pending_remote_ack: true"#,
                        escape_graphql_string(&at.to_rfc3339())
                    )
                })
                .unwrap_or_default();
            let omission_fields = exact_omission_fields_fragment(&omission);
            let mutation = format!(
                r#"mutation {{
                    update_AgentToolCall(
                        filter: {{ _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }} }},
                        input: {{
                            result: "{}",
                            status: "{}",
                            lifecycle_state: "cancelled",
                            cancel_cause: "{}",
                            started_at: "{}",
                            deadline_at: "{}",
                            completed_at: "{}",
                            {omission_fields}
                            latency_ms: {}
                            {remote_cancel_intent_fragment}
                            {}
                        }}
                    ) {{ _docID }}
                }}"#,
                escape_graphql_string(&doc_id),
                escape_graphql_string(detail),
                escape_graphql_string(&self.terminal_persistence_status(Some(completion_reason))),
                cause.as_str(),
                started_at.to_rfc3339(),
                self.deadline_at.to_rfc3339(),
                now.to_rfc3339(),
                (now - started_at).num_milliseconds(),
                self.clear_unclaimed_deadline_fragment(),
            );
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Running,
                &[ExactToolEvidence {
                    collection: "AgentToolOutputOmission",
                    exact: &omission,
                    require_execution_owner: true,
                }],
                &mutation,
                "update_AgentToolCall",
                "cancel_during_run_with_exact_omission",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::Cancelled;
                    self.cancel_cause = Some(cause);
                    return Ok(true);
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_running_compare("cancel_during_run")
                        .await?;
                    return Ok(false);
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(next_omission) = self
                        .retain_terminal_omission_fact_or_adopt(
                            ToolCallState::Running,
                            ToolCallState::Cancelled,
                            super::super::evidence::ToolOutputOmissionReason::Cancelled,
                            detail,
                            "cancel_during_run",
                        )
                        .await?
                    else {
                        return Ok(false);
                    };
                    omission = next_omission;
                }
                ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                    "AgentToolCall {doc_id} kept changing while binding cancellation omission"
                ),
            }
        }
        unreachable!("bounded exact-omission loop returns on every outcome")
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
