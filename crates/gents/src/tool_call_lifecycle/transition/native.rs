use super::*;

impl ToolCallLifecycle {
    fn requester_did_fragment(&self) -> String {
        crate::session::requester_did_create_field(self.requester_did.as_deref())
    }

    /// GraphQL fragment for the durable workflow-group projection fields,
    /// emitted on every create path so a bridge stays projectable by
    /// `workflow_group_id` regardless of which transition created its row.
    /// Empty for non-workflow tool calls (back-compat with older rows).
    fn workflow_fields_fragment(&self) -> String {
        match (
            self.workflow_group_id.as_deref(),
            self.workflow_role.as_deref(),
        ) {
            (Some(group_id), Some(role)) if !group_id.is_empty() && !role.is_empty() => {
                let escaped_group_id = escape_graphql_string(group_id);
                let escaped_role = escape_graphql_string(role);
                format!(
                    r#"workflow_group_id: "{escaped_group_id}",
                    workflow_role: "{escaped_role}","#
                )
            }
            _ => String::new(),
        }
    }

    /// Persist the accepted invocation/execution genesis before any terminal
    /// disposition can be written. This gives pre-dispatch failures and
    /// cancellations an exact signed `pending` version to which their typed
    /// omission fact can bind.
    async fn persist_pending(&mut self) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "persist_pending")?;
        if let Some(doc_id) = self.doc_id.as_deref() {
            crate::document_version::verified_current_signed_document_version(
                &self.node,
                "AgentToolCall",
                doc_id,
            )
            .await?;
            return Ok(());
        }
        if let Some(existing) =
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, &self.tool_call_id).await?
        {
            let immutable_match = existing.request_id == self.request_id
                && existing.session_id == self.session_id
                && existing.agent_did == self.agent_did
                && existing.requester_did == self.requester_did
                && existing.message_sequence == self.message_sequence
                && existing.tool_name == self.tool_name
                && existing.args == self.args
                && existing.await_mode == self.await_mode
                && existing.cancel_policy == self.cancel_policy
                && existing.child_request_id == self.child_request_id
                && existing.spawn_target_did == self.spawn_target_did
                && existing.workflow_group_id == self.workflow_group_id
                && existing.workflow_role == self.workflow_role;
            if !immutable_match || existing.state != ToolCallState::Pending {
                anyhow::bail!(
                    "AgentToolCall {}:{} replay conflicts with the accepted pending execution",
                    self.session_id,
                    self.tool_call_id
                );
            }
            let existing_doc_id = existing
                .doc_id
                .ok_or_else(|| anyhow!("accepted pending AgentToolCall has no _docID"))?;
            crate::document_version::verified_current_signed_document_version(
                &self.node,
                "AgentToolCall",
                &existing_doc_id,
            )
            .await?;
            self.doc_id = Some(existing_doc_id);
            return Ok(());
        }

        let deadline_at_str = self.deadline_at.to_rfc3339();
        let escaped_request_id = escape_graphql_string(&self.request_id);
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_agent_did = escape_graphql_string(&self.agent_did);
        let escaped_tool_call_id = escape_graphql_string(&self.tool_call_id);
        let escaped_tool_name = escape_graphql_string(&self.tool_name);
        let escaped_args = escape_graphql_string(&self.args);
        let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
        let message_sequence = self.message_sequence;
        let await_mode_str = self.await_mode.as_str();
        let cancel_policy_str = self.cancel_policy.as_str();
        let child_field = self
            .child_request_id
            .as_ref()
            .map(|child| format!(r#"child_request_id: "{}","#, escape_graphql_string(child)))
            .unwrap_or_default();
        let spawn_target_field = self
            .spawn_target_did
            .as_ref()
            .map(|did| format!(r#"spawn_target_did: "{}","#, escape_graphql_string(did)))
            .unwrap_or_default();
        let unclaimed_deadline_field = self
            .unclaimed_deadline_at
            .map(|deadline| {
                format!(
                    r#"unclaimed_deadline_at: "{}","#,
                    escape_graphql_string(
                        &deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    )
                )
            })
            .unwrap_or_default();
        let requester_did_field = self.requester_did_fragment();
        let workflow_fields = self.workflow_fields_fragment();
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    request_id: "{escaped_request_id}",
                    session_id: "{escaped_session_id}",
                    agent_did: "{escaped_agent_did}",
                    {requester_did_field}
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "called",
                    lifecycle_state: "pending",
                    started_at: null,
                    deadline_at: "{deadline_at_str}",
                    {child_field}
                    {spawn_target_field}
                    {unclaimed_deadline_field}
                    await_mode: "{await_mode_str}",
                    cancel_policy: "{cancel_policy_str}",
                    {workflow_fields}
                    selected_service_id: null,
                    selected_tool_name: null,
                    tool_failure_class: null,
                    latency_ms: null
                }}) {{ _docID }}
            }}"#
        );
        let response = execute_mutation_with_retry(&self.node, &mutation, "persist_pending")
            .await
            .context("persist pending AgentToolCall")?;
        let created_doc_id = extract_doc_id_from_create_response(&response)
            .ok_or_else(|| anyhow!("create pending AgentToolCall returned no _docID"))?;
        let admitted =
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, &self.tool_call_id)
                .await?
                .ok_or_else(|| anyhow!("created pending AgentToolCall disappeared"))?;
        if admitted.doc_id.as_deref() != Some(created_doc_id.as_str())
            || admitted.state != ToolCallState::Pending
        {
            anyhow::bail!(
                "created pending AgentToolCall did not remain the sole accepted logical execution"
            );
        }
        crate::document_version::verified_current_signed_document_version(
            &self.node,
            "AgentToolCall",
            &created_doc_id,
        )
        .await?;
        self.doc_id = Some(created_doc_id);
        Ok(())
    }

    /// Pending → Running. Persists an exact pending genesis first, then
    /// advances that physical execution. Idempotent if already Running.
    pub async fn start_running(&mut self) -> Result<()> {
        if self.state == ToolCallState::Running {
            // Idempotent re-entry (retry path).
            let doc_id = self
                .doc_id
                .as_deref()
                .ok_or_else(|| anyhow!("running AgentToolCall has no _docID"))?;
            self.require_current_signed_execution(doc_id).await?;
            return Ok(());
        }
        self.ensure_state(&[ToolCallState::Pending], "start_running")?;

        self.persist_pending().await?;
        let doc_id = self
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("pending AgentToolCall has no _docID"))?;
        let now = Utc::now();
        let started_at_str = now.to_rfc3339();
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        lifecycle_state: "running",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}"
                    }}
                ) {{ _docID }}
            }}"#
        );

        match execute_transition_from_signed_source(
            &self.node,
            doc_id,
            &self.agent_did,
            ToolCallState::Pending,
            &mutation,
            "update_AgentToolCall",
            "start_running_from_exact_signed_pending",
        )
        .await?
        {
            ExactEvidenceTransitionOutcome::Applied(_) => {}
            ExactEvidenceTransitionOutcome::Lost => {
                let current = ToolCallLifecycle::load(
                    self.node.clone(),
                    &self.session_id,
                    &self.tool_call_id,
                )
                .await?
                .ok_or_else(|| anyhow!("pending AgentToolCall disappeared during dispatch"))?;
                if current.doc_id.as_deref() != Some(doc_id)
                    || current.agent_did != self.agent_did
                    || current.state != ToolCallState::Running
                {
                    anyhow::bail!("pending AgentToolCall left pending before dispatch could start");
                }
                crate::document_version::verified_current_signed_document_version(
                    &self.node,
                    "AgentToolCall",
                    doc_id,
                )
                .await?;
            }
            ExactEvidenceTransitionOutcome::Stale => {
                unreachable!("signed-source transitions classify changed phases as lost")
            }
        }

        self.state = ToolCallState::Running;
        self.started_at = Some(now);
        Ok(())
    }

    /// Running → Completed. The full output fact is durably published first;
    /// the terminal compare-and-set then binds that exact signed version.
    pub async fn complete(&mut self, result: &str) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "complete")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeCompleteOnSubagentTool.into());
        }
        let Some(output) = self
            .retain_terminal_output_fact_or_adopt(ToolCallState::Running, "complete", result)
            .await?
        else {
            return Ok(());
        };
        self.complete_with_result_fact(result, &output).await
    }

    /// Complete using a canonical full-output fact that the caller already
    /// published (for example, the truncation boundary that retained the full
    /// bytes and chose a bounded model-facing projection).
    pub(crate) async fn complete_with_result_fact(
        &mut self,
        result: &str,
        output: &crate::SignedDocumentVersionRef,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "complete_with_result_fact")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeCompleteOnSubagentTool.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("complete called before start_running persisted a row"))?
            .clone();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("complete called without started_at set"))?;
        let mut exact_output = output.clone();
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let latency_ms = (now - started_at).num_milliseconds();
            let escaped_result = escape_graphql_string(result);
            let escaped_doc_id = escape_graphql_string(&doc_id);
            let now_str = now.to_rfc3339();
            let started_at_str = started_at.to_rfc3339();
            let deadline_at_str = self.deadline_at.to_rfc3339();
            let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();
            let output_fields = exact_result_fields_fragment(&exact_output);
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
                            {output_fields}
                            latency_ms: {latency_ms}
                            {unclaimed_deadline_clear}
                        }}
                    ) {{ _docID }}
                }}"#
            );
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Running,
                &[ExactToolEvidence {
                    collection: "AgentToolResult",
                    exact: &exact_output,
                    require_execution_owner: true,
                }],
                &mutation,
                "update_AgentToolCall",
                "complete_with_exact_output",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::Completed;
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_running_compare("complete").await?;
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(output) = self
                        .republish_terminal_output_fact_or_adopt(
                            ToolCallState::Running,
                            "complete",
                            &exact_output,
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    exact_output = output;
                }
                ExactEvidenceTransitionOutcome::Stale => {
                    anyhow::bail!(
                        "AgentToolCall {doc_id} kept changing while binding exact completion output"
                    );
                }
            }
        }
        unreachable!("bounded exact-output loop returns on every outcome")
    }

    pub(super) async fn retain_terminal_output_fact(
        &self,
        output: &str,
    ) -> Result<crate::SignedDocumentVersionRef> {
        let metadata = serde_json::json!({
            "truncated": false,
            "truncated_by": null,
            "original_lines": output.lines().count(),
            "original_bytes": output.len(),
            "projection": "lifecycle_terminal"
        })
        .to_string();
        crate::truncation::DefraSpillTruncator::new(
            self.node.clone(),
            &self.agent_did,
            &self.session_id,
        )
        .with_requester_did(self.requester_did.clone())
        .with_tool_call_id(&self.tool_call_id)
        .retain_full_output_fact(&self.tool_name, &self.args, output, &metadata, None, false)
        .await
    }

    pub(super) async fn retain_terminal_output_fact_or_adopt(
        &mut self,
        expected_source: ToolCallState,
        method: &'static str,
        output: &str,
    ) -> Result<Option<crate::SignedDocumentVersionRef>> {
        match self.retain_terminal_output_fact(output).await {
            Ok(output) => Ok(Some(output)),
            Err(error) => {
                if self.adopt_if_source_moved(expected_source, method).await? {
                    return Ok(None);
                }
                Err(error)
            }
        }
    }

    async fn republish_terminal_output_fact_or_adopt(
        &mut self,
        expected_source: ToolCallState,
        method: &'static str,
        source: &crate::SignedDocumentVersionRef,
    ) -> Result<Option<crate::SignedDocumentVersionRef>> {
        let publish = crate::truncation::DefraSpillTruncator::new(
            self.node.clone(),
            &self.agent_did,
            &self.session_id,
        )
        .with_requester_did(self.requester_did.clone())
        .with_tool_call_id(&self.tool_call_id)
        .retain_full_output_fact_from_exact(source)
        .await;
        match publish {
            Ok(output) => Ok(Some(output)),
            Err(error) => {
                if self.adopt_if_source_moved(expected_source, method).await? {
                    return Ok(None);
                }
                Err(error)
            }
        }
    }

    pub(super) async fn retain_terminal_omission_fact_or_adopt(
        &mut self,
        expected_source: ToolCallState,
        terminal: ToolCallState,
        reason: super::super::evidence::ToolOutputOmissionReason,
        detail: &str,
        method: &'static str,
    ) -> Result<Option<crate::SignedDocumentVersionRef>> {
        match self
            .retain_terminal_omission_fact(expected_source, terminal, reason, detail)
            .await
        {
            Ok(omission) => Ok(Some(omission)),
            Err(error) => {
                if self.adopt_if_source_moved(expected_source, method).await? {
                    return Ok(None);
                }
                Err(error)
            }
        }
    }

    /// Running → Failed. For tool errors during execution. Sets failure_class.
    pub async fn fail(&mut self, result: &str, failure: super::FailureClass) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "fail")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeFailOnSubagentTool.into());
        }
        let Some(output) = self
            .retain_terminal_output_fact_or_adopt(ToolCallState::Running, "fail", result)
            .await?
        else {
            return Ok(());
        };
        self.fail_with_details(result, failure, None, &output).await
    }

    pub(crate) async fn fail_with_result_fact(
        &mut self,
        result: &str,
        failure: super::FailureClass,
        output: &crate::SignedDocumentVersionRef,
    ) -> Result<()> {
        self.fail_with_details(result, failure, None, output).await
    }

    pub(crate) async fn fail_with_command_denial_and_result_fact(
        &mut self,
        result: &str,
        denial: &CommandPolicyDenial,
        output: &crate::SignedDocumentVersionRef,
    ) -> Result<()> {
        self.fail_with_details(result, FailureClass::PolicyDenied, Some(denial), output)
            .await
    }

    async fn fail_with_details(
        &mut self,
        result: &str,
        failure: super::FailureClass,
        command_denial: Option<&CommandPolicyDenial>,
        output: &crate::SignedDocumentVersionRef,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Running], "fail")?;
        if self.is_bridge() {
            return Err(IllegalToolCallTransition::NativeFailOnSubagentTool.into());
        }

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("fail called before start_running persisted a row"))?
            .clone();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("fail called without started_at set"))?;
        let mut exact_output = output.clone();
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let latency_ms = (now - started_at).num_milliseconds();
            let escaped_result = escape_graphql_string(result);
            let escaped_doc_id = escape_graphql_string(&doc_id);
            let now_str = now.to_rfc3339();
            let failure_class_str = failure.as_str();
            let started_at_str = started_at.to_rfc3339();
            let deadline_at_str = self.deadline_at.to_rfc3339();
            let unclaimed_deadline_clear = self.clear_unclaimed_deadline_fragment();
            let command_denial_fields = command_denial_fields_fragment(command_denial);
            let output_fields = exact_result_fields_fragment(&exact_output);
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
                            lifecycle_state: "failed",
                            started_at: "{started_at_str}",
                            deadline_at: "{deadline_at_str}",
                            completed_at: "{now_str}",
                            tool_failure_class: "{failure_class_str}",
                            {command_denial_fields}
                            {output_fields}
                            latency_ms: {latency_ms}
                            {unclaimed_deadline_clear}
                        }}
                    ) {{ _docID }}
                }}"#
            );
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Running,
                &[ExactToolEvidence {
                    collection: "AgentToolResult",
                    exact: &exact_output,
                    require_execution_owner: true,
                }],
                &mutation,
                "update_AgentToolCall",
                "fail_with_exact_output",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::Failed;
                    self.failure_class = Some(failure);
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_running_compare("fail").await?;
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(output) = self
                        .republish_terminal_output_fact_or_adopt(
                            ToolCallState::Running,
                            "fail",
                            &exact_output,
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    exact_output = output;
                }
                ExactEvidenceTransitionOutcome::Stale => {
                    anyhow::bail!(
                        "AgentToolCall {doc_id} kept changing while binding exact failure output"
                    );
                }
            }
        }
        unreachable!("bounded exact-output loop returns on every outcome")
    }

    /// Pending → Failed. Used when the dispatcher cannot start the call
    /// (MCP service unreachable, argument parse failure pre-spawn).
    pub async fn spawn_failed(&mut self, failure: super::FailureClass, reason: &str) -> Result<()> {
        self.spawn_failed_with_details(failure, reason, None).await
    }

    #[allow(dead_code)]
    pub(crate) async fn spawn_failed_with_command_denial(
        &mut self,
        reason: &str,
        denial: &CommandPolicyDenial,
    ) -> Result<()> {
        self.spawn_failed_with_details(FailureClass::PolicyDenied, reason, Some(denial))
            .await
    }

    async fn spawn_failed_with_details(
        &mut self,
        failure: super::FailureClass,
        reason: &str,
        command_denial: Option<&CommandPolicyDenial>,
    ) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "spawn_failed")?;
        self.persist_pending().await?;
        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("pending AgentToolCall has no _docID"))?
            .clone();
        let Some(mut omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::Pending,
                ToolCallState::Failed,
                super::super::evidence::ToolOutputOmissionReason::PreDispatchFailure,
                reason,
                "spawn_failed",
            )
            .await?
        else {
            return Ok(());
        };
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let escaped_doc_id = escape_graphql_string(&doc_id);
            let escaped_result = escape_graphql_string(reason);
            let failure_class_str = failure.as_str();
            let command_denial_fields = command_denial_fields_fragment(command_denial);
            let omission_fields = exact_omission_fields_fragment(&omission);
            let mutation = format!(
                r#"mutation {{
                    update_AgentToolCall(
                        filter: {{
                            _docID: {{ _eq: "{escaped_doc_id}" }},
                            lifecycle_state: {{ _eq: "pending" }}
                        }},
                        input: {{
                            result: "{escaped_result}",
                            status: "completed",
                            lifecycle_state: "failed",
                            started_at: null,
                            deadline_at: "{}",
                            completed_at: "{}",
                            tool_failure_class: "{failure_class_str}",
                            {command_denial_fields}
                            {omission_fields}
                            latency_ms: 0
                        }}
                    ) {{ _docID }}
                }}"#,
                self.deadline_at.to_rfc3339(),
                now.to_rfc3339(),
            );
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Pending,
                &[ExactToolEvidence {
                    collection: "AgentToolOutputOmission",
                    exact: &omission,
                    require_execution_owner: true,
                }],
                &mutation,
                "update_AgentToolCall",
                "spawn_failed_with_exact_omission",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::Failed;
                    self.failure_class = Some(failure);
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_pending_compare("spawn_failed").await?;
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(next_omission) = self
                        .retain_terminal_omission_fact_or_adopt(
                            ToolCallState::Pending,
                            ToolCallState::Failed,
                            super::super::evidence::ToolOutputOmissionReason::PreDispatchFailure,
                            reason,
                            "spawn_failed",
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    omission = next_omission;
                }
                ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                    "AgentToolCall {doc_id} kept changing while binding pre-dispatch omission"
                ),
            }
        }
        unreachable!("bounded exact-omission loop returns on every outcome")
    }

    /// Running → TimedOut. Called by the runtime deadline wrapper and startup
    /// recovery when a running tool call exceeds its effective deadline.
    ///
    /// Returns whether this caller won the durable running-state compare.
    /// A loser adopts the already-terminal durable row (another actor —
    /// interrupt, recovery sweep, or the tool itself — terminalized first),
    /// preserving that terminal's state and recorded cause.
    pub async fn timeout(&mut self) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "timeout")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("timeout called before start_running persisted a row"))?
            .clone();
        let started_at = self
            .started_at
            .ok_or_else(|| anyhow!("timeout called without started_at set"))?;
        let detail = format!(
            "tool call deadline exceeded at {}",
            self.deadline_at.to_rfc3339()
        );
        let Some(mut omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::Running,
                ToolCallState::TimedOut,
                super::super::evidence::ToolOutputOmissionReason::TimedOut,
                &detail,
                "timeout",
            )
            .await?
        else {
            return Ok(false);
        };
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let omission_fields = exact_omission_fields_fragment(&omission);
            let mutation = format!(
                r#"mutation {{
                    update_AgentToolCall(
                        filter: {{ _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }} }},
                        input: {{
                            result: "{}",
                            status: "completed",
                            lifecycle_state: "timedOut",
                            tool_failure_class: "{}",
                            cancel_cause: "{}",
                            started_at: "{}",
                            deadline_at: "{}",
                            completed_at: "{}",
                            {omission_fields}
                            latency_ms: {}
                            {}
                        }}
                    ) {{ _docID }}
                }}"#,
                escape_graphql_string(&doc_id),
                escape_graphql_string(&detail),
                FailureClass::External.as_str(),
                CancelCause::Deadline.as_str(),
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
                "timeout_with_exact_omission",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::TimedOut;
                    self.failure_class = Some(FailureClass::External);
                    self.cancel_cause = Some(CancelCause::Deadline);
                    return Ok(true);
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_running_compare("timeout").await?;
                    return Ok(false);
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(next_omission) = self
                        .retain_terminal_omission_fact_or_adopt(
                            ToolCallState::Running,
                            ToolCallState::TimedOut,
                            super::super::evidence::ToolOutputOmissionReason::TimedOut,
                            &detail,
                            "timeout",
                        )
                        .await?
                    else {
                        return Ok(false);
                    };
                    omission = next_omission;
                }
                ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                    "AgentToolCall {doc_id} kept changing while binding timeout omission"
                ),
            }
        }
        unreachable!("bounded exact-omission loop returns on every outcome")
    }

    /// Pending → Cancelled. Used when a tool call is cancelled before
    /// dispatch creates a running row.
    ///
    pub async fn cancel_before_dispatch(&mut self, cause: CancelCause) -> Result<()> {
        self.ensure_state(&[ToolCallState::Pending], "cancel_before_dispatch")?;
        self.persist_pending().await?;
        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| anyhow!("pending AgentToolCall has no _docID"))?
            .clone();
        let detail = "tool call cancelled before dispatch";
        let Some(mut omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::Pending,
                ToolCallState::Cancelled,
                super::super::evidence::ToolOutputOmissionReason::Cancelled,
                detail,
                "cancel_before_dispatch",
            )
            .await?
        else {
            return Ok(());
        };
        for stale_retry in 0..=crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES {
            let now = Utc::now();
            let omission_fields = exact_omission_fields_fragment(&omission);
            let mutation = format!(
                r#"mutation {{
                    update_AgentToolCall(
                        filter: {{ _docID: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "pending" }} }},
                        input: {{
                            result: "{}",
                            status: "completed",
                            lifecycle_state: "cancelled",
                            cancel_cause: "{}",
                            started_at: null,
                            deadline_at: "{}",
                            completed_at: "{}",
                            {omission_fields}
                            latency_ms: 0
                        }}
                    ) {{ _docID }}
                }}"#,
                escape_graphql_string(&doc_id),
                escape_graphql_string(detail),
                cause.as_str(),
                self.deadline_at.to_rfc3339(),
                now.to_rfc3339(),
            );
            match execute_transition_with_exact_evidence(
                &self.node,
                &doc_id,
                ToolCallState::Pending,
                &[ExactToolEvidence {
                    collection: "AgentToolOutputOmission",
                    exact: &omission,
                    require_execution_owner: true,
                }],
                &mutation,
                "update_AgentToolCall",
                "cancel_before_dispatch_with_exact_omission",
            )
            .await?
            {
                ExactEvidenceTransitionOutcome::Applied(_) => {
                    self.state = ToolCallState::Cancelled;
                    self.cancel_cause = Some(cause);
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Lost => {
                    self.sync_after_lost_pending_compare("cancel_before_dispatch")
                        .await?;
                    return Ok(());
                }
                ExactEvidenceTransitionOutcome::Stale
                    if stale_retry < crate::retry::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    let Some(next_omission) = self
                        .retain_terminal_omission_fact_or_adopt(
                            ToolCallState::Pending,
                            ToolCallState::Cancelled,
                            super::super::evidence::ToolOutputOmissionReason::Cancelled,
                            detail,
                            "cancel_before_dispatch",
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    omission = next_omission;
                }
                ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                    "AgentToolCall {doc_id} kept changing while binding pre-dispatch cancellation omission"
                ),
            }
        }
        unreachable!("bounded exact-omission loop returns on every outcome")
    }

    /// Pending → AwaitingApproval. Persists the row held for an operator
    /// verdict; the tool is NOT dispatched and `started_at` stays null until
    /// `approve_and_start`. Mirrors the Lean `holdForApproval` transition.
    pub async fn hold_for_approval(&mut self) -> Result<()> {
        if self.state == ToolCallState::AwaitingApproval {
            // Idempotent re-entry (retry path).
            let doc_id = self
                .doc_id
                .as_deref()
                .ok_or_else(|| anyhow!("held AgentToolCall has no _docID"))?;
            self.require_current_signed_execution(doc_id).await?;
            return Ok(());
        }
        self.ensure_state(&[ToolCallState::Pending], "hold_for_approval")?;
        self.persist_pending().await?;
        let doc_id = self
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("pending AgentToolCall has no _docID"))?;
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }}, lifecycle_state: {{ _eq: "pending" }} }},
                    input: {{ lifecycle_state: "awaitingApproval", started_at: null, deadline_at: "{deadline_at_str}" }}
                ) {{ _docID }}
            }}"#
        );

        match execute_transition_from_signed_source(
            &self.node,
            doc_id,
            &self.agent_did,
            ToolCallState::Pending,
            &mutation,
            "update_AgentToolCall",
            "hold_for_approval_from_exact_signed_pending",
        )
        .await?
        {
            ExactEvidenceTransitionOutcome::Applied(_) => {}
            ExactEvidenceTransitionOutcome::Lost => {
                self.sync_after_lost_pending_compare("hold_for_approval")
                    .await?;
                if self.state != ToolCallState::AwaitingApproval {
                    return Ok(());
                }
            }
            ExactEvidenceTransitionOutcome::Stale => {
                unreachable!("signed-source transitions classify changed phases as lost")
            }
        }

        self.state = ToolCallState::AwaitingApproval;
        Ok(())
    }

    async fn sync_after_lost_pending_compare(&mut self, method: &'static str) -> Result<()> {
        let current =
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, &self.tool_call_id)
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "{method} compare failed and AgentToolCall row disappeared for session_id={} tool_call_id={}",
                        self.session_id,
                        self.tool_call_id
                    )
                })?;
        if current.state == ToolCallState::Pending {
            anyhow::bail!(
                "{method} compare failed but AgentToolCall row is still pending for session_id={} tool_call_id={}",
                self.session_id,
                self.tool_call_id
            );
        }
        let current_doc_id = current
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("{method} current AgentToolCall has no _docID"))?;
        self.require_current_signed_execution(current_doc_id)
            .await?;
        self.doc_id = current.doc_id;
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        Ok(())
    }

    /// Reload after a lost held-row compare (the row left `awaitingApproval`
    /// under us — cancelled or timed out by another actor). Adopts current
    /// row state so the caller can observe the terminal.
    async fn sync_after_lost_held_compare(&mut self, method: &'static str) -> Result<()> {
        let current =
            ToolCallLifecycle::load(self.node.clone(), &self.session_id, &self.tool_call_id)
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "{method} compare failed and AgentToolCall row disappeared for session_id={} tool_call_id={}",
                        self.session_id,
                        self.tool_call_id
                    )
                })?;
        if current.state == ToolCallState::AwaitingApproval {
            anyhow::bail!(
                "{method} compare failed but AgentToolCall row is still awaitingApproval for session_id={} tool_call_id={}",
                self.session_id,
                self.tool_call_id
            );
        }
        let current_doc_id = current
            .doc_id
            .as_deref()
            .ok_or_else(|| anyhow!("{method} current AgentToolCall has no _docID"))?;
        self.require_current_signed_execution(current_doc_id)
            .await?;
        self.doc_id = current.doc_id;
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        Ok(())
    }

    /// AwaitingApproval → Running on approved evidence. Sets `started_at`
    /// (the Lean `approve` transition's startedAt discipline). Returns false
    /// when the compare-and-set loses (row already left awaitingApproval).
    pub async fn approve_and_start(
        &mut self,
        approval: &crate::SignedDocumentVersionRef,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::AwaitingApproval], "approve_and_start")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| {
                anyhow!("approve_and_start called before hold_for_approval persisted a row")
            })?
            .clone();
        let now = Utc::now();
        let escaped_doc_id = escape_graphql_string(&doc_id);
        let started_at_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let approval_fields = exact_approval_fields_fragment(approval);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "awaitingApproval" }}
                    }},
                    input: {{
                        {approval_fields}
                        lifecycle_state: "running",
                        started_at: "{started_at_str}",
                        deadline_at: "{deadline_at_str}"
                    }}
                ) {{ _docID }}
            }}"#
        );

        match execute_transition_with_exact_evidence(
            &self.node,
            &doc_id,
            ToolCallState::AwaitingApproval,
            &[ExactToolEvidence {
                collection: "AgentToolApproval",
                exact: approval,
                require_execution_owner: false,
            }],
            &mutation,
            "update_AgentToolCall",
            "approve_and_start_with_exact_approval",
        )
        .await?
        {
            ExactEvidenceTransitionOutcome::Applied(_) => {
                self.state = ToolCallState::Running;
                self.started_at = Some(now);
                Ok(true)
            }
            ExactEvidenceTransitionOutcome::Lost => {
                self.sync_after_lost_held_compare("approve_and_start")
                    .await?;
                Ok(false)
            }
            ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                "held AgentToolCall {doc_id} changed after its approval evidence was signed"
            ),
        }
    }

    /// AwaitingApproval → Failed on denied evidence. Sets
    /// `failure_class = approvalDenied` (the Lean `deny` transition). Returns
    /// false when the compare-and-set loses.
    pub async fn deny_approval(
        &mut self,
        reason: &str,
        approval: &crate::SignedDocumentVersionRef,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::AwaitingApproval], "deny_approval")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| {
                anyhow!("deny_approval called before hold_for_approval persisted a row")
            })?
            .clone();
        let Some(omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::AwaitingApproval,
                ToolCallState::Failed,
                super::super::evidence::ToolOutputOmissionReason::ApprovalDenied,
                reason,
                "deny_approval",
            )
            .await?
        else {
            return Ok(false);
        };
        let now = Utc::now();
        let escaped_doc_id = escape_graphql_string(&doc_id);
        let escaped_result = escape_graphql_string(reason);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let failure_class_str = FailureClass::ApprovalDenied.as_str();
        let approval_fields = exact_approval_fields_fragment(approval);
        let omission_fields = exact_omission_fields_fragment(&omission);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "awaitingApproval" }}
                    }},
                    input: {{
                        {approval_fields}
                        {omission_fields}
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "failed",
                        tool_failure_class: "{failure_class_str}",
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: 0
                    }}
                ) {{ _docID }}
            }}"#
        );

        match execute_transition_with_exact_evidence(
            &self.node,
            &doc_id,
            ToolCallState::AwaitingApproval,
            &[
                ExactToolEvidence {
                    collection: "AgentToolApproval",
                    exact: approval,
                    require_execution_owner: false,
                },
                ExactToolEvidence {
                    collection: "AgentToolOutputOmission",
                    exact: &omission,
                    require_execution_owner: true,
                },
            ],
            &mutation,
            "update_AgentToolCall",
            "deny_approval_with_exact_evidence",
        )
        .await?
        {
            ExactEvidenceTransitionOutcome::Applied(_) => {
                self.state = ToolCallState::Failed;
                self.failure_class = Some(FailureClass::ApprovalDenied);
                Ok(true)
            }
            ExactEvidenceTransitionOutcome::Lost => {
                self.sync_after_lost_held_compare("deny_approval").await?;
                Ok(false)
            }
            ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                "held AgentToolCall {doc_id} changed after denial evidence was signed"
            ),
        }
    }

    /// AwaitingApproval → Cancelled (the Lean `cancelWhileHeld` transition).
    /// Returns false when the compare-and-set loses.
    pub async fn cancel_while_held(&mut self, cause: CancelCause) -> Result<bool> {
        self.ensure_state(&[ToolCallState::AwaitingApproval], "cancel_while_held")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| {
                anyhow!("cancel_while_held called before hold_for_approval persisted a row")
            })?
            .clone();
        let detail = "tool call cancelled while awaiting approval";
        let Some(omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::AwaitingApproval,
                ToolCallState::Cancelled,
                super::super::evidence::ToolOutputOmissionReason::Cancelled,
                detail,
                "cancel_while_held",
            )
            .await?
        else {
            return Ok(false);
        };
        let now = Utc::now();
        let escaped_doc_id = escape_graphql_string(&doc_id);
        let escaped_result = escape_graphql_string(detail);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let cancel_cause = cause.as_str();
        let omission_fields = exact_omission_fields_fragment(&omission);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "awaitingApproval" }}
                    }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "cancelled",
                        cancel_cause: "{cancel_cause}",
                        {omission_fields}
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: 0
                    }}
                ) {{ _docID }}
            }}"#
        );

        match execute_transition_with_exact_evidence(
            &self.node,
            &doc_id,
            ToolCallState::AwaitingApproval,
            &[ExactToolEvidence {
                collection: "AgentToolOutputOmission",
                exact: &omission,
                require_execution_owner: true,
            }],
            &mutation,
            "update_AgentToolCall",
            "cancel_while_held_with_exact_omission",
        )
        .await?
        {
            ExactEvidenceTransitionOutcome::Applied(_) => {
                self.state = ToolCallState::Cancelled;
                self.cancel_cause = Some(cause);
                Ok(true)
            }
            ExactEvidenceTransitionOutcome::Lost => {
                self.sync_after_lost_held_compare("cancel_while_held")
                    .await?;
                Ok(false)
            }
            ExactEvidenceTransitionOutcome::Stale => anyhow::bail!(
                "held AgentToolCall {doc_id} changed while binding cancellation omission"
            ),
        }
    }

    /// AwaitingApproval → TimedOut when the deadline expires unanswered (the
    /// Lean `timeoutWhileHeld` transition). Returns false when the
    /// compare-and-set loses.
    pub async fn timeout_while_held(&mut self) -> Result<bool> {
        self.ensure_state(&[ToolCallState::AwaitingApproval], "timeout_while_held")?;

        let doc_id = self
            .doc_id
            .as_ref()
            .ok_or_else(|| {
                anyhow!("timeout_while_held called before hold_for_approval persisted a row")
            })?
            .clone();
        let detail = format!(
            "tool call approval deadline exceeded at {}",
            self.deadline_at.to_rfc3339()
        );
        let Some(omission) = self
            .retain_terminal_omission_fact_or_adopt(
                ToolCallState::AwaitingApproval,
                ToolCallState::TimedOut,
                super::super::evidence::ToolOutputOmissionReason::TimedOut,
                &detail,
                "timeout_while_held",
            )
            .await?
        else {
            return Ok(false);
        };
        let now = Utc::now();
        let escaped_doc_id = escape_graphql_string(&doc_id);
        let escaped_result = escape_graphql_string(&detail);
        let now_str = now.to_rfc3339();
        // DefraDB requires DateTime fields to be re-supplied on update.
        let deadline_at_str = self.deadline_at.to_rfc3339();
        let failure_class = FailureClass::External.as_str();
        let cancel_cause = CancelCause::Deadline.as_str();
        let omission_fields = exact_omission_fields_fragment(&omission);

        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "awaitingApproval" }}
                    }},
                    input: {{
                        result: "{escaped_result}",
                        status: "completed",
                        lifecycle_state: "timedOut",
                        tool_failure_class: "{failure_class}",
                        cancel_cause: "{cancel_cause}",
                        {omission_fields}
                        deadline_at: "{deadline_at_str}",
                        completed_at: "{now_str}",
                        latency_ms: 0
                    }}
                ) {{ _docID }}
            }}"#
        );

        match execute_transition_with_exact_evidence(
            &self.node,
            &doc_id,
            ToolCallState::AwaitingApproval,
            &[ExactToolEvidence {
                collection: "AgentToolOutputOmission",
                exact: &omission,
                require_execution_owner: true,
            }],
            &mutation,
            "update_AgentToolCall",
            "timeout_while_held_with_exact_omission",
        )
        .await?
        {
            ExactEvidenceTransitionOutcome::Applied(_) => {
                self.state = ToolCallState::TimedOut;
                self.failure_class = Some(FailureClass::External);
                self.cancel_cause = Some(CancelCause::Deadline);
                Ok(true)
            }
            ExactEvidenceTransitionOutcome::Lost => {
                self.sync_after_lost_held_compare("timeout_while_held")
                    .await?;
                Ok(false)
            }
            ExactEvidenceTransitionOutcome::Stale => {
                anyhow::bail!("held AgentToolCall {doc_id} changed while binding timeout omission")
            }
        }
    }
}
