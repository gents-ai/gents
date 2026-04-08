use super::*;

impl RequestLifecycle {
    pub fn set_response_doc_id(&mut self, doc_id: &str) {
        self.ensure_state(
            &[LocalLifecycleState::Claimed, LocalLifecycleState::Streaming],
            "set_response_doc_id",
        )
        .expect("response doc can only be attached after claim");
        self.response_doc_id = Some(doc_id.to_string());
        self.state = LocalLifecycleState::Streaming;
    }

    pub async fn claim(&mut self) -> Result<ClaimOutcome> {
        self.claim_inner(false).await
    }

    pub async fn claim_with_identity(&mut self) -> Result<ClaimOutcome> {
        self.claim_inner(true).await
    }

    pub async fn mark_slot_acquired(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Claimed], "mark_slot_acquired")?;
        self.transition_execution_view(
            "processing",
            PersistedLifecycleState::Claimed,
            PersistedAdmissionState::Waiting,
            "processing",
            PersistedLifecycleState::Claimed,
            PersistedAdmissionState::Acquired,
        )
        .await
    }

    pub async fn begin_execution(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Claimed], "begin_execution")?;
        self.transition_execution_view(
            "processing",
            PersistedLifecycleState::Claimed,
            PersistedAdmissionState::Acquired,
            "processing",
            PersistedLifecycleState::Processing,
            PersistedAdmissionState::Executing,
        )
        .await
    }

    async fn claim_inner(&mut self, explicit_did: bool) -> Result<ClaimOutcome> {
        self.ensure_state(&[LocalLifecycleState::Pending], "claim")?;
        let dedup = self.check_deduplication().await?;
        if !dedup.is_earliest {
            self.mark_superseded_pending_request(dedup.blocking_request_id.as_deref())
                .await?;
            self.state = LocalLifecycleState::Superseded;
            return Ok(ClaimOutcome::Superseded);
        }

        let now = chrono::Utc::now();
        let claimed_at = now.to_rfc3339();
        let deadline =
            (now + chrono::Duration::seconds(self.deadline_duration_secs as i64)).to_rfc3339();
        let doc_id = &self.request.doc_id;
        let escaped_claimed_at = escape_graphql_string(&claimed_at);
        let escaped_deadline = escape_graphql_string(&deadline);
        let escaped_backend_id = escape_graphql_string(&self.backend_id);
        let execution_origin = self.execution_origin.as_str();

        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "processing",
                        lifecycle_state: "{lifecycle_state}",
                        admission_state: "{admission_state}",
                        backend_id: "{escaped_backend_id}",
                        execution_origin: "{execution_origin}",
                        claimed_at: "{escaped_claimed_at}",
                        deadline: "{escaped_deadline}"
                    }}
                ) {{ _docID }}
            }}"#,
            lifecycle_state = PersistedLifecycleState::Claimed.as_str(),
            admission_state = PersistedAdmissionState::Waiting.as_str(),
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!("claiming request failed: {:?}", resp.errors);
        }

        if !resp
            .data
            .as_ref()
            .and_then(|d| d.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            match self.request_status().await? {
                Some(status) if status == "processing" => {
                    tracing::debug!(doc_id = %doc_id, "claimed via post-update verification");
                }
                Some(status) => {
                    anyhow::bail!("request {} is no longer pending (status={status})", doc_id)
                }
                None => anyhow::bail!("request {} disappeared while claiming", doc_id),
            }
        } else {
            tracing::debug!(
                doc_id = %doc_id,
                deadline = %deadline,
                backend_id = %self.backend_id,
                execution_origin,
                "claimed agent request with deadline"
            );
        }

        if explicit_did {
            session::upsert_conversation_from_request_with_did(
                &self.node,
                &self.request.session_id,
                &self.agent_name,
                &self.agent_did,
                &self.request.request_id,
                &self.request.content,
                "processing",
            )
            .await?;
        } else {
            session::upsert_conversation_from_request(
                &self.node,
                &self.request.session_id,
                &self.agent_name,
                &self.request.request_id,
                &self.request.content,
                "processing",
            )
            .await?;
        }

        self.suppress_later_pending_duplicates(&dedup.duplicates_to_suppress)
            .await?;
        self.state = LocalLifecycleState::Claimed;

        Ok(ClaimOutcome::Claimed)
    }
}
