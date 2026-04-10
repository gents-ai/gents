use super::rows::{DedupRow, RequestStatusTransition};
use super::*;

impl RequestLifecycle {
    pub async fn record_failure_reason(&mut self, reason: &str) -> Result<()> {
        let doc_id = &self.request.doc_id;
        let escaped_reason = escape_graphql_string(reason);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ failure_reason: "{escaped_reason}" }}
                ) {{ _docID }}
            }}"#
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!(
                "recording failure reason for request {} doc_id={doc_id}: {:?}",
                self.request.request_id,
                resp.errors
            );
        }

        self.failure_reason = Some(reason.to_string());
        Ok(())
    }

    pub async fn advance(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Streaming], "advance")?;
        let doc_id = self
            .response_doc_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("advance() called before response doc created"))?;
        let next_progress_seq = self.progress_seq + 1;

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

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!(
                "failed to advance progress_seq for doc_id={doc_id} progress_seq={next_progress_seq}: {:?}",
                resp.errors
            );
        }

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
        if self.state == LocalLifecycleState::Superseded {
            tracing::info!(
                request_id = %self.request.request_id,
                "skipping complete() because request lifecycle was superseded"
            );
            return Ok(());
        }
        self.ensure_state(
            &[LocalLifecycleState::Claimed, LocalLifecycleState::Streaming],
            "complete",
        )?;

        match self
            .transition_request_status(
                "processing",
                "completed",
                PersistedLifecycleState::Completed,
                PersistedAdmissionState::Released,
            )
            .await?
        {
            RequestStatusTransition::Updated | RequestStatusTransition::AlreadyTarget => {
                match session::update_conversation_status_if_latest_with_identity(
                    &self.node,
                    &self.request.session_id,
                    &self.agent_name,
                    &self.agent_did,
                    &self.behavior_id,
                    &self.request.request_id,
                    "completed",
                )
                .await?
                {
                    session::ConversationUpdateOutcome::Updated => {}
                    session::ConversationUpdateOutcome::AlreadyApplied => {
                        tracing::debug!(
                            session_id = %self.request.session_id,
                            request_id = %self.request.request_id,
                            "conversation already marked completed for latest request"
                        );
                    }
                    session::ConversationUpdateOutcome::SkippedStaleRequest => {
                        tracing::info!(
                            session_id = %self.request.session_id,
                            request_id = %self.request.request_id,
                            "skipping stale conversation completion for non-latest request"
                        );
                    }
                }
            }
            RequestStatusTransition::ConflictingTerminal(current) => {
                tracing::info!(
                    request_id = %self.request.request_id,
                    current_status = %current,
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
        if self.state == LocalLifecycleState::Superseded {
            tracing::info!(
                request_id = %self.request.request_id,
                "skipping fail() because request lifecycle was superseded"
            );
            return Ok(());
        }
        self.ensure_state(
            &[LocalLifecycleState::Claimed, LocalLifecycleState::Streaming],
            "fail",
        )?;

        match self
            .transition_request_status(
                "processing",
                "error",
                PersistedLifecycleState::Failed,
                PersistedAdmissionState::Released,
            )
            .await?
        {
            RequestStatusTransition::Updated | RequestStatusTransition::AlreadyTarget => {
                match session::update_conversation_status_if_latest_with_identity(
                    &self.node,
                    &self.request.session_id,
                    &self.agent_name,
                    &self.agent_did,
                    &self.behavior_id,
                    &self.request.request_id,
                    "active",
                )
                .await?
                {
                    session::ConversationUpdateOutcome::Updated => {}
                    session::ConversationUpdateOutcome::AlreadyApplied => {
                        tracing::debug!(
                            session_id = %self.request.session_id,
                            request_id = %self.request.request_id,
                            "conversation already active for latest request"
                        );
                    }
                    session::ConversationUpdateOutcome::SkippedStaleRequest => {
                        tracing::info!(
                            session_id = %self.request.session_id,
                            request_id = %self.request.request_id,
                            "skipping stale conversation reset for non-latest request"
                        );
                    }
                }
            }
            RequestStatusTransition::ConflictingTerminal(current) => {
                tracing::info!(
                    request_id = %self.request.request_id,
                    current_status = %current,
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

    pub(super) async fn transition_request_status(
        &self,
        from_status: &str,
        target_status: &str,
        target_lifecycle_state: PersistedLifecycleState,
        target_admission_state: PersistedAdmissionState,
    ) -> Result<RequestStatusTransition> {
        let doc_id = &self.request.doc_id;
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "{from_status}" }}
                    }},
                    input: {{
                        status: "{target_status}",
                        lifecycle_state: "{target_lifecycle_state}",
                        admission_state: "{target_admission_state}",
                        behavior_id: "{behavior_id}",
                        backend_id: "{backend_id}",
                        execution_origin: "{execution_origin}",
                        failure_reason: "{failure_reason}"
                    }}
                ) {{ _docID }}
            }}"#,
            target_lifecycle_state = target_lifecycle_state.as_str(),
            target_admission_state = target_admission_state.as_str(),
            behavior_id = escape_graphql_string(&self.behavior_id),
            backend_id = escape_graphql_string(&self.backend_id),
            execution_origin = self.execution_origin.as_str(),
            failure_reason =
                escape_graphql_string(self.failure_reason.as_deref().unwrap_or_default()),
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!(
                "updating request status {} -> {} for doc_id={doc_id}: {:?}",
                from_status,
                target_status,
                resp.errors
            );
        }

        if resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            tracing::debug!(
                doc_id = %doc_id,
                from_status = %from_status,
                target_status = %target_status,
                "updated request status"
            );
            return Ok(RequestStatusTransition::Updated);
        }

        match self.request_status().await? {
            Some(current) if current == target_status => Ok(RequestStatusTransition::AlreadyTarget),
            Some(current) if matches!(current.as_str(), "completed" | "error" | "superseded") => {
                Ok(RequestStatusTransition::ConflictingTerminal(current))
            }
            Some(current) => anyhow::bail!(
                "request {} could not transition {} -> {}; current status={}",
                self.request.request_id,
                from_status,
                target_status,
                current
            ),
            None => anyhow::bail!(
                "request {} disappeared while transitioning {} -> {}",
                self.request.request_id,
                from_status,
                target_status
            ),
        }
    }

    pub(super) async fn transition_execution_view(
        &self,
        from_status: &str,
        from_lifecycle_state: PersistedLifecycleState,
        from_admission_state: PersistedAdmissionState,
        target_status: &str,
        target_lifecycle_state: PersistedLifecycleState,
        target_admission_state: PersistedAdmissionState,
    ) -> Result<()> {
        let doc_id = &self.request.doc_id;
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "{from_status}" }},
                        lifecycle_state: {{ _eq: "{from_lifecycle_state}" }},
                        admission_state: {{ _eq: "{from_admission_state}" }}
                    }},
                    input: {{
                        status: "{target_status}",
                        lifecycle_state: "{target_lifecycle_state}",
                        admission_state: "{target_admission_state}",
                        behavior_id: "{behavior_id}",
                        backend_id: "{backend_id}",
                        execution_origin: "{execution_origin}",
                        failure_reason: "{failure_reason}"
                    }}
                ) {{ _docID }}
            }}"#,
            from_lifecycle_state = from_lifecycle_state.as_str(),
            from_admission_state = from_admission_state.as_str(),
            target_lifecycle_state = target_lifecycle_state.as_str(),
            target_admission_state = target_admission_state.as_str(),
            behavior_id = escape_graphql_string(&self.behavior_id),
            backend_id = escape_graphql_string(&self.backend_id),
            execution_origin = self.execution_origin.as_str(),
            failure_reason =
                escape_graphql_string(self.failure_reason.as_deref().unwrap_or_default()),
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!(
                "updating execution view {} / {} -> {} / {} for doc_id={doc_id}: {:?}",
                from_lifecycle_state.as_str(),
                from_admission_state.as_str(),
                target_lifecycle_state.as_str(),
                target_admission_state.as_str(),
                resp.errors
            );
        }

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
            Some(current)
                if current.status == target_status
                    && current.lifecycle_state.as_deref() == Some(target_lifecycle_state.as_str())
                    && current.admission_state.as_deref() == Some(target_admission_state.as_str()) =>
            {
                Ok(())
            }
            Some(current) => anyhow::bail!(
                "request {} could not transition execution view {} / {} -> {} / {}; current status={} lifecycle_state={} admission_state={}",
                self.request.request_id,
                from_lifecycle_state.as_str(),
                from_admission_state.as_str(),
                target_lifecycle_state.as_str(),
                target_admission_state.as_str(),
                current.status,
                current.lifecycle_state.as_deref().unwrap_or("missing"),
                current.admission_state.as_deref().unwrap_or("missing")
            ),
            None => anyhow::bail!(
                "request {} disappeared while transitioning execution view {} / {} -> {} / {}",
                self.request.request_id,
                from_lifecycle_state.as_str(),
                from_admission_state.as_str(),
                target_lifecycle_state.as_str(),
                target_admission_state.as_str()
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

    pub(super) async fn suppress_later_pending_duplicates(
        &self,
        duplicates: &[DedupRow],
    ) -> Result<()> {
        let superseded_by_request = escape_graphql_string(&self.request.request_id);
        for duplicate in duplicates {
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            _docID: {{ _eq: "{doc_id}" }},
                            status: {{ _eq: "pending" }}
                        }},
                        input: {{
                            status: "superseded",
                            lifecycle_state: "{lifecycle_state}",
                            admission_state: "{admission_state}",
                            superseded_by_request: "{superseded_by_request}",
                            behavior_id: "{behavior_id}",
                            backend_id: "{backend_id}",
                            execution_origin: "{execution_origin}"
                        }}
                    ) {{ _docID }}
                }}"#,
                doc_id = duplicate.doc_id,
                lifecycle_state = PersistedLifecycleState::Superseded.as_str(),
                admission_state = PersistedAdmissionState::Released.as_str(),
                superseded_by_request = superseded_by_request,
                behavior_id = escape_graphql_string(&self.behavior_id),
                backend_id = escape_graphql_string(&self.backend_id),
                execution_origin = self.execution_origin.as_str(),
            );

            let resp = self.node.execute(&mutation).await;
            if resp.has_errors() {
                anyhow::bail!(
                    "failed to suppress duplicate request_id={} doc_id={}: {:?}",
                    duplicate.request_id,
                    duplicate.doc_id,
                    resp.errors
                );
            }

            if resp
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentRequest"))
                .is_some_and(response_has_documents)
            {
                tracing::info!(
                    session_id = %self.request.session_id,
                    claimed_request_id = %self.request.request_id,
                    duplicate_request_id = %duplicate.request_id,
                    "marked later duplicate pending request superseded"
                );
            }
        }

        Ok(())
    }

    pub(super) async fn mark_superseded_pending_request(
        &self,
        superseded_by_request: Option<&str>,
    ) -> Result<()> {
        let superseded_by_request = escape_graphql_string(superseded_by_request.unwrap_or(""));
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "superseded",
                        lifecycle_state: "{lifecycle_state}",
                        admission_state: "{admission_state}",
                        superseded_by_request: "{superseded_by_request}",
                        behavior_id: "{behavior_id}",
                        backend_id: "{backend_id}",
                        execution_origin: "{execution_origin}"
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = self.request.doc_id,
            lifecycle_state = PersistedLifecycleState::Superseded.as_str(),
            admission_state = PersistedAdmissionState::Released.as_str(),
            superseded_by_request = superseded_by_request,
            behavior_id = escape_graphql_string(&self.behavior_id),
            backend_id = escape_graphql_string(&self.backend_id),
            execution_origin = self.execution_origin.as_str(),
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!(
                "failed to mark duplicate request_id={} superseded: {:?}",
                self.request.request_id,
                resp.errors
            );
        }

        Ok(())
    }
}
