use super::*;

async fn fetch_interrupt_and_ttl(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                interrupt_requested_at
                valid_until
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("fetch_interrupt_and_ttl for {doc_id}: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array());
    let row = rows
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("AgentRequest {doc_id} not found"))?;
    let interrupt = row
        .get("interrupt_requested_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let valid = row
        .get("valid_until")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok((interrupt, valid))
}

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

    pub async fn begin_execution(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Claimed], "begin_execution")?;
        self.transition_execution_view(
            "processing",
            PersistedLifecycleState::Claimed,
            "processing",
            PersistedLifecycleState::Processing,
        )
        .await
    }

    async fn transition_pending_to_interrupted(&mut self, _interrupt_at: &str) -> Result<()> {
        let doc_id = &self.request.doc_id;
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, status: {{ _eq: "pending" }} }},
                    input: {{
                        status: "interrupted",
                        lifecycle_state: "interrupted"
                    }}
                ) {{ _docID }}
            }}"#
        );
        session::execute_mutation_with_retry(&self.node, &mutation, "interrupt_before_claim")
            .await?;
        Ok(())
    }

    async fn transition_pending_to_dead_stale(&mut self) -> Result<()> {
        let doc_id = &self.request.doc_id;
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, status: {{ _eq: "pending" }} }},
                    input: {{
                        status: "dead",
                        lifecycle_state: "dead",
                        failure_reason: "Stale"
                    }}
                ) {{ _docID }}
            }}"#
        );
        session::execute_mutation_with_retry(&self.node, &mutation, "expire_stale").await?;
        self.failure_reason = Some("Stale".to_string());
        Ok(())
    }

    async fn claim_inner(&mut self, _explicit_did: bool) -> Result<ClaimOutcome> {
        self.ensure_state(&[LocalLifecycleState::Pending], "claim")?;
        let dedup = self.check_deduplication().await?;
        if !dedup.is_earliest {
            self.mark_superseded_pending_request(dedup.blocking_request_id.as_deref())
                .await?;
            self.state = LocalLifecycleState::Superseded;
            return Ok(ClaimOutcome::Superseded);
        }

        let (interrupt_requested_at, valid_until) =
            fetch_interrupt_and_ttl(&self.node, &self.request.doc_id).await?;

        // Tie-break: interrupt always wins over stale
        if let Some(interrupt_at) = interrupt_requested_at {
            self.transition_pending_to_interrupted(&interrupt_at)
                .await?;
            self.state = LocalLifecycleState::Interrupted;
            return Ok(ClaimOutcome::Interrupted);
        }

        let valid_until_at_claim = match valid_until.as_deref() {
            Some(s) => {
                let dt = chrono::DateTime::parse_from_rfc3339(s)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "invalid valid_until on request {}: {e}",
                            self.request.doc_id
                        )
                    })?
                    .with_timezone(&chrono::Utc);
                if chrono::Utc::now() > dt {
                    self.transition_pending_to_dead_stale().await?;
                    self.state = LocalLifecycleState::Dead;
                    return Ok(ClaimOutcome::Expired);
                }
                Some(dt)
            }
            None => None,
        };

        let now = chrono::Utc::now();
        let claimed_at = now.to_rfc3339();
        let deadline_at = now + chrono::Duration::seconds(self.deadline_duration_secs as i64);
        let deadline = deadline_at.to_rfc3339();
        let doc_id = &self.request.doc_id;
        let escaped_claimed_at = escape_graphql_string(&claimed_at);
        let escaped_deadline = escape_graphql_string(&deadline);
        let escaped_backend_id = escape_graphql_string(&self.backend_id);
        let escaped_behavior_id = escape_graphql_string(&self.behavior_id);
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
                        behavior_id: "{escaped_behavior_id}",
                        backend_id: "{escaped_backend_id}",
                        execution_origin: "{execution_origin}",
                        claimed_at: "{escaped_claimed_at}",
                        deadline: "{escaped_deadline}"
                    }}
                ) {{ _docID }}
            }}"#,
            lifecycle_state = PersistedLifecycleState::Claimed.as_str(),
        );

        let resp =
            session::execute_mutation_with_retry(&self.node, &mutation, "claim_request").await?;

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

        self.suppress_later_pending_duplicates(&dedup.duplicates_to_suppress)
            .await?;
        self.state = LocalLifecycleState::Claimed;
        self.claimed_deadline_at = Some(deadline_at);
        self.valid_until_at_claim = valid_until_at_claim;

        Ok(ClaimOutcome::Claimed)
    }
}
