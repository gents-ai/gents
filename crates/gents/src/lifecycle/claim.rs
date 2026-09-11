use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BackgroundCompletionClaimSnapshot {
    through_sequence: Option<u32>,
    notification_keys: Vec<String>,
}

impl BackgroundCompletionClaimSnapshot {
    fn mutation_fields(&self) -> String {
        let through_sequence = self
            .through_sequence
            .map(|sequence| format!("background_completion_input_through_sequence: {sequence},"))
            .unwrap_or_default();
        let keys_json = serde_json::to_string(&self.notification_keys)
            .expect("background completion notification keys serialize");
        format!(
            r#"{through_sequence}
                background_completion_notification_keys_json: "{}","#,
            escape_graphql_string(&keys_json)
        )
    }
}

async fn claim_request_with_projection<F>(
    node: &EmbeddedNode,
    session_id: &str,
    capture_background_snapshot: bool,
    request: &AgentRequest,
    claimed_at: &str,
    build_mutation: F,
) -> Result<(defra_node::QueryResponse, BackgroundCompletionClaimSnapshot)>
where
    F: Fn(&str) -> String + Sync,
{
    let session_id = escape_graphql_string(session_id);
    let snapshot_query = format!(
        r#"{{
            all_messages: AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
            notifications: AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    message_key: {{ _like: "background-completion-notification:%" }}
                }},
                order: {{ sequence: ASC }}
            ) {{ sequence message_key }}
        }}"#
    );
    let snapshot_query = &snapshot_query;
    let build_mutation = &build_mutation;
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "lifecycle.claim_request",
        move |txn| {
            Box::pin(async move {
                let snapshot = if capture_background_snapshot {
                    let response = txn.execute_local_response(&snapshot_query).await?;
                    let through_sequence = response
                        .data
                        .as_ref()
                        .and_then(|data| data.get("all_messages"))
                        .and_then(serde_json::Value::as_array)
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("sequence"))
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|sequence| u32::try_from(sequence).ok());
                    let notification_keys = response
                        .data
                        .as_ref()
                        .and_then(|data| data.get("notifications"))
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|row| {
                            row.get("sequence")
                                .and_then(serde_json::Value::as_u64)
                                .zip(through_sequence.map(u64::from))
                                .is_some_and(|(sequence, cutoff)| sequence <= cutoff)
                        })
                        .filter_map(|row| {
                            row.get("message_key").and_then(serde_json::Value::as_str)
                        })
                        .map(ToOwned::to_owned)
                        .collect();
                    BackgroundCompletionClaimSnapshot {
                        through_sequence,
                        notification_keys,
                    }
                } else {
                    BackgroundCompletionClaimSnapshot::default()
                };
                let snapshot_fields = capture_background_snapshot
                    .then(|| snapshot.mutation_fields())
                    .unwrap_or_default();
                let mutation = build_mutation(&snapshot_fields);
                let claimed = txn.execute_local_response(&mutation).await?;
                if claimed
                    .data
                    .as_ref()
                    .and_then(|data| data.get("update_AgentRequest"))
                    .is_some_and(response_has_documents)
                {
                    super::materialize::apply_request_session_projection(&txn, request, claimed_at)
                        .await?;
                }
                Ok::<_, anyhow::Error>((claimed, snapshot))
            })
        },
    )
    .await
}

fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

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
                request_id
                interrupt_requested_at
                valid_until
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("fetch_interrupt_and_ttl for {doc_id}: {:?}", resp.errors);
    }
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&resp, "AgentRequest")?
            .ok_or_else(|| anyhow::anyhow!("AgentRequest {doc_id} not found"))?;
    let interrupt = row
        .interrupt_requested_at
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from);
    let valid = row
        .valid_until
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok((interrupt, valid))
}

impl RequestLifecycle {
    fn set_response_doc_id(&mut self, doc_id: &str) {
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

    /// The durable request transition and response creation commit together.
    /// Cancellation before commit leaves Claimed/absent for lease recovery.
    pub async fn begin_owned_execution(
        &mut self,
        stream_writer: &crate::streaming::DefraStreamWriter,
    ) -> Result<String> {
        self.ensure_state(&[LocalLifecycleState::Claimed], "begin_owned_execution")?;
        let generation = self.execution_generation()?.to_string();
        let doc_id = stream_writer
            .begin_owned_response(
                &self.request.session_id,
                &self.request.request_id,
                &self.request.doc_id,
                &self.behavior_id,
                self.request.requester_did.as_deref(),
                &generation,
                self.execution_lease_duration_secs,
            )
            .await?;
        self.set_response_doc_id(&doc_id);
        Ok(doc_id)
    }

    async fn transition_pending_to_interrupted(&mut self, _interrupt_at: &str) -> Result<()> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        lifecycle_state: "interrupted",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let resp = crate::config_client::ConfigAccess::write_local_idempotent_update_response(
            &self.node,
            "interrupt_before_claim",
            &mutation,
        )
        .await?;
        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            let request_view = self.request_view().await?;
            if request_view
                .as_ref()
                .is_some_and(|row| row.lifecycle_state == Some(RequestLifecycleState::Interrupted))
            {
                return Ok(());
            }
            anyhow::bail!(
                "request {} could not transition pending -> interrupted; current lifecycle_state={}",
                self.request.request_id,
                request_view
                    .as_ref()
                    .and_then(|row| row.lifecycle_state)
                    .map(RequestLifecycleState::as_str)
                    .unwrap_or("missing")
            );
        }
        Ok(())
    }

    async fn transition_pending_to_dead_stale(&mut self) -> Result<()> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        lifecycle_state: "dead",
                        failure_reason: "Stale",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let resp = crate::config_client::ConfigAccess::write_local_idempotent_update_response(
            &self.node,
            "expire_stale",
            &mutation,
        )
        .await?;
        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            let request_view = self.request_view().await?;
            if request_view
                .as_ref()
                .is_some_and(|row| row.lifecycle_state == Some(RequestLifecycleState::Dead))
            {
                return Ok(());
            }
            anyhow::bail!(
                "request {} could not transition pending -> dead; current lifecycle_state={}",
                self.request.request_id,
                request_view
                    .as_ref()
                    .and_then(|row| row.lifecycle_state)
                    .map(RequestLifecycleState::as_str)
                    .unwrap_or("missing")
            );
        }
        self.failure_reason = Some("Stale".to_string());
        Ok(())
    }

    pub async fn reject_admission(&mut self, reason: &str) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Pending], "reject_admission")?;
        let request_doc_id = escape_graphql_string(&self.request.doc_id);
        let request_id = escape_graphql_string(&self.request.request_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let behavior_id = escape_graphql_string(&self.behavior_id);
        let session_id = escape_graphql_string(&self.request.session_id);
        let reason_text = reason.to_string();
        let reason = escape_graphql_string(&reason_text);
        let content = escape_graphql_string(&format!("Error: {reason_text}"));
        let terminalized_at_value = chrono::Utc::now().to_rfc3339();
        let terminalized_at = escape_graphql_string(&terminalized_at_value);
        let requester_did_field =
            session::requester_did_create_field(self.request.requester_did.as_deref());
        let request_mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{request_doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        lifecycle_state: "failed",
                        failure_reason: "{reason}",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response_mutation = format!(
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
                    created_at: "{terminalized_at}",
                    completed_at: "{terminalized_at}"
                }}) {{ _docID }}
            }}"#
        );

        let request_mutation = &request_mutation;
        let response_mutation = &response_mutation;
        let updated = crate::config_client::ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            crate::config_client::IdempotentTransactionRetry::Standard,
            "lifecycle.reject_admission",
            move |txn| {
                Box::pin(async move {
                    let response = txn.execute_local_response(&request_mutation).await?;
                    let updated = response
                        .data
                        .as_ref()
                        .and_then(|data| data.get("update_AgentRequest"))
                        .is_some_and(response_has_documents);
                    let response_doc_id = if updated {
                        let response = txn.execute_local_response(&response_mutation).await?;
                        Some(
                            extract_single_doc_id(&response, "create_AgentResponse").ok_or_else(
                                || {
                                    anyhow::anyhow!(
                                        "admission rejection created no AgentResponse document"
                                    )
                                },
                            )?,
                        )
                    } else {
                        None
                    };
                    Ok::<_, anyhow::Error>((updated, response_doc_id))
                })
            },
        )
        .await?;

        let (updated, response_doc_id) = updated;
        if !updated {
            let request_view = self.request_view().await?;
            if !request_view
                .as_ref()
                .is_some_and(|row| row.lifecycle_state == Some(RequestLifecycleState::Failed))
            {
                anyhow::bail!(
                    "request {} could not reject admission from lifecycle_state={}",
                    self.request.request_id,
                    request_view
                        .as_ref()
                        .and_then(|row| row.lifecycle_state)
                        .map(RequestLifecycleState::as_str)
                        .unwrap_or("missing")
                );
            }
        }
        self.response_doc_id = response_doc_id;
        self.failure_reason = Some(reason_text);
        self.state = LocalLifecycleState::Failed;
        Ok(())
    }

    async fn claim_inner(&mut self, _explicit_did: bool) -> Result<ClaimOutcome> {
        self.ensure_state(&[LocalLifecycleState::Pending], "claim")?;
        let (interrupt_requested_at, valid_until) =
            fetch_interrupt_and_ttl(&self.node, &self.request.doc_id).await?;

        if let Some(interrupt_at) = interrupt_requested_at {
            self.transition_pending_to_interrupted(&interrupt_at)
                .await?;
            self.state = LocalLifecycleState::Interrupted;
            return Ok(ClaimOutcome::Interrupted);
        }

        let valid_until_at_claim =
            match super::parse_valid_until(valid_until.as_deref(), chrono::Utc::now()) {
                super::TtlOutcome::Malformed(error) => {
                    anyhow::bail!(
                        "invalid valid_until on request {}: {error}",
                        self.request.doc_id
                    );
                }
                super::TtlOutcome::Expired(_) => {
                    self.transition_pending_to_dead_stale().await?;
                    self.state = LocalLifecycleState::Dead;
                    return Ok(ClaimOutcome::Expired);
                }
                super::TtlOutcome::NotSet => None,
                super::TtlOutcome::Live(parsed) => Some(parsed),
            };

        let dedup = self.check_deduplication().await?;
        if !dedup.is_earliest {
            tracing::info!(
                request_id = %self.request.request_id,
                session_id = %self.request.session_id,
                blocking_request_id = dedup.blocking_request_id.as_deref().unwrap_or(""),
                "request remains queued behind earlier same-session request"
            );
            return Ok(ClaimOutcome::Queued);
        }

        let now = chrono::Utc::now();
        let claimed_at = now.to_rfc3339();
        let execution_generation = uuid::Uuid::new_v4().to_string();
        let execution_lease_expires_at =
            now + chrono::Duration::seconds(self.execution_lease_duration_secs as i64);
        let synthesized_deadline_at =
            now + chrono::Duration::seconds(self.deadline_duration_secs as i64);
        let deadline_at = self
            .request
            .deadline
            .as_deref()
            .and_then(parse_rfc3339_utc)
            .unwrap_or(synthesized_deadline_at);
        let deadline = deadline_at.to_rfc3339();
        let doc_id = self.request.doc_id.clone();
        let escaped_doc_id = escape_graphql_string(&doc_id);
        let escaped_claimed_at = escape_graphql_string(&claimed_at);
        let escaped_execution_generation = escape_graphql_string(&execution_generation);
        let escaped_execution_lease_expires_at =
            escape_graphql_string(&execution_lease_expires_at.to_rfc3339());
        let escaped_deadline = escape_graphql_string(&deadline);
        let escaped_backend_id = escape_graphql_string(&self.backend_id);
        let execution_origin = self.execution_origin.as_str();
        let request_fields = crate::watcher::AGENT_REQUEST_FIELDS;

        // The generation is written on every successful claim and never cleared
        // when the same physical request resumes. It distinguishes an unclaimed
        // request from a previously pinned unlimited (null) budget.
        let prior_generation = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(&self.request.execution_generation)?,
        )?;
        let budget_field = if self.request.execution_generation.is_none() {
            let limit = self
                .configured_max_total_tokens
                .map(|limit| {
                    anyhow::ensure!(limit > 0, "configured max_total_tokens must be positive");
                    i64::try_from(limit).map_err(anyhow::Error::from)
                })
                .transpose()?;
            format!(
                "max_total_tokens: {},",
                gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(limit)?)?
            )
        } else {
            // Do not rewrite the durable allowance, including explicit zero or
            // unlimited null, from edited configuration on reclaim.
            crate::completion_factory::parse_aggregate_token_limit(self.request.max_total_tokens)?;
            String::new()
        };

        let build_mutation = |background_completion_snapshot_fields: &str| {
            format!(
                r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        lifecycle_state: {{ _eq: "pending" }},
                        execution_generation: {{ _eq: {prior_generation} }}
                    }},
                    input: {{
                        lifecycle_state: "{lifecycle_state}",
                        backend_id: "{escaped_backend_id}",
                        claimed_at: "{escaped_claimed_at}",
                        execution_generation: "{escaped_execution_generation}",
                        execution_lease_expires_at: "{escaped_execution_lease_expires_at}",
                        execution_progress_seq: 0,
                        {budget_field}
                        {background_completion_snapshot_fields}
                        deadline: "{escaped_deadline}"
                    }}
                ) {{{request_fields}
                    lifecycle_state
                    interrupt_requested_at
                    valid_until
                    _version {{ cid height fieldName }}
                }}
            }}"#,
                lifecycle_state = RequestLifecycleState::Claimed.as_str(),
            )
        };

        let is_background_completion =
            crate::lifecycle::is_background_completion_request(&self.request.input);
        let (resp, snapshot) = claim_request_with_projection(
            self.node.as_ref(),
            &self.request.session_id,
            is_background_completion,
            &self.request,
            &claimed_at,
            &build_mutation,
        )
        .await?;
        let background_completion_input_through_sequence = snapshot.through_sequence;

        // The mutation response is the only response that can carry the exact
        // commit produced by this claim. Do not fall back to a post-update
        // query: another branchable write may win that read and falsely bind
        // this lifecycle to a commit it did not author.
        let claimed_request =
            crate::watcher::agent_request_from_mutation_response(&resp, "update_AgentRequest")?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "request {doc_id} was not pending when this runtime attempted to claim it"
                    )
                })?;
        let request_commit_cid =
            crate::graphql::mutation_composite_version(&resp, "update_AgentRequest")?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "claimed AgentRequest {doc_id} returned no composite DefraDB version"
                    )
                })?
                .cid;
        if claimed_request.doc_id != doc_id {
            anyhow::bail!(
                "claim for AgentRequest {doc_id} returned document {}",
                claimed_request.doc_id
            );
        }
        self.request = claimed_request;
        self.request_commit_cid = Some(request_commit_cid);
        tracing::debug!(
            doc_id = %doc_id,
            deadline = %deadline,
            backend_id = %self.backend_id,
            execution_origin,
            "claimed agent request with exact DefraDB version"
        );

        self.state = LocalLifecycleState::Claimed;
        self.claimed_deadline_at = Some(deadline_at);
        self.background_completion_input_through_sequence =
            background_completion_input_through_sequence;
        self.valid_until_at_claim = valid_until_at_claim;
        self.execution_lease = Some(RequestExecutionLease::new(execution_generation));

        Ok(ClaimOutcome::Claimed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const TEST_AGENT_DID: &str = "did:test:claim-order-test";
    const TEST_BEHAVIOR_ID: &str = "general";
    const TEST_BACKEND_ID: &str = "backend-order";

    async fn test_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        node
    }

    async fn insert_pending_request(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        created_at: &str,
        input: Option<&gents_protocol::request_input::RequestInput>,
        execution_origin: &str,
    ) -> AgentRequest {
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_created_at = escape_graphql_string(created_at);
        let input_literal =
            gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(input).unwrap())
                .unwrap();
        let escaped_execution_origin = escape_graphql_string(execution_origin);
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{escaped_request_id}",
                    agent_did: "{TEST_AGENT_DID}",
                    behavior_id: "{TEST_BEHAVIOR_ID}",
                    session_id: "{escaped_session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{escaped_request_id}",
                    superseded_by_request: "",
                    content: "same-session request",
                    input: {input_literal},
                    lifecycle_state: "pending",
                    backend_id: "",
                    execution_origin: "{escaped_execution_origin}",
                    failure_reason: "",
                    created_at: "{escaped_created_at}",
                    retry_count: 0,
                    max_retries: {max_retries},
                    subagent_depth: 0
                }}) {{ {fields} }}
            }}"#,
            max_retries = DEFAULT_REQUEST_MAX_RETRIES,
            fields = crate::watcher::AGENT_REQUEST_FIELDS,
        );
        let response = crate::config_client::ConfigAccess::write_local_response(
            node,
            "test.insert_pending_request",
            &mutation,
        )
        .await
        .unwrap();
        crate::watcher::agent_request_from_mutation_response(&response, "create_AgentRequest")
            .expect("decode created request")
            .expect("created request receipt")
    }

    #[tokio::test]
    async fn claim_preserves_same_session_ordering() {
        let node = test_node().await;
        let first = insert_pending_request(
            node.as_ref(),
            "same-session-request-1",
            "same-session",
            "2026-01-01T00:00:00Z",
            None,
            "interactive",
        )
        .await;
        let first_doc_id = first.doc_id.clone();
        let refresh = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{first_doc_id}" }} }},
                    input: {{ content: "refreshed before claim" }}
                ) {{ _docID }}
            }}"#
        );
        let refresh_response = node.execute(&refresh).await;
        assert!(
            refresh_response.has_errors(),
            "immutable request content changed"
        );
        let second = insert_pending_request(
            node.as_ref(),
            "same-session-request-2",
            "same-session",
            "2026-01-01T00:00:01Z",
            None,
            "interactive",
        )
        .await;

        let mut first_lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            first,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        let mut second_lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            second,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );

        assert_eq!(
            first_lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        assert_eq!(
            first_lifecycle.request().content,
            "same-session request",
            "claim must preserve immutable request semantics"
        );
        let claimed_cid = first_lifecycle
            .request_commit_cid()
            .expect("successful claim records its exact DefraDB commit")
            .to_string();

        let commits = crate::graphql::composite_commits(
            node.as_ref(),
            &first_doc_id,
            "verify_exact_claim_version",
        )
        .await
        .unwrap();
        assert_eq!(
            commits.first().map(|commit| commit.cid.as_str()),
            Some(claimed_cid.as_str()),
            "the recorded claim CID must remain the latest durable request version"
        );
        assert!(
            commits.iter().any(|commit| commit.cid == claimed_cid),
            "the recorded claim CID must be a real composite commit"
        );
        assert_eq!(
            second_lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Queued
        );
    }

    #[tokio::test]
    async fn background_claim_snapshots_transcript_before_successor_input() {
        let node = test_node().await;
        let session_id = "background-claim-snapshot";
        use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
        let input = RequestInput {
            queue: Some(RequestQueue {
                source: QueueSource::BackgroundCompletion,
                policy: QueuePolicy::Coalesce,
                key: Some(format!("background_completion:{session_id}")),
                queued_after_request_id: Some("parent-request".to_string()),
                interrupted_request_id: None,
                background_completion_wake_version: Some(1),
            }),
            ..Default::default()
        };
        let request = insert_pending_request(
            node.as_ref(),
            "background-wake-1",
            session_id,
            "2026-08-12T22:00:00Z",
            Some(&input),
            "scheduled",
        )
        .await;
        session::append_message_once_with_key_and_requester_did(
            node.as_ref(),
            session_id,
            TEST_AGENT_DID,
            None,
            "user",
            "first notification",
            None,
            Some(&request.request_id),
            Some(&request.doc_id),
            "background-completion-notification:child-1:subagent",
            Some(1),
        )
        .await
        .unwrap();
        session::save_message(
            node.as_ref(),
            session_id,
            TEST_AGENT_DID,
            2,
            "assistant",
            "prior response",
            None,
        )
        .await
        .unwrap();

        let request_doc_id = request.doc_id.clone();
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            request,
            60,
            ExecutionOrigin::Scheduled,
            TEST_BACKEND_ID,
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        assert_eq!(
            lifecycle.background_completion_input_through_sequence(),
            Some(2)
        );
        let snapshot = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                    background_completion_input_through_sequence
                    background_completion_notification_keys_json
                }} }}"#,
                escape_graphql_string(&request_doc_id)
            ))
            .await;
        assert!(
            !snapshot.has_errors(),
            "snapshot query: {:?}",
            snapshot.errors
        );
        let row = &snapshot.data.as_ref().unwrap()["AgentRequest"][0];
        assert_eq!(
            row["background_completion_input_through_sequence"].as_i64(),
            Some(2)
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(
                row["background_completion_notification_keys_json"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            vec!["background-completion-notification:child-1:subagent"]
        );

        session::save_message(
            node.as_ref(),
            session_id,
            TEST_AGENT_DID,
            3,
            "user",
            "successor notification",
            None,
        )
        .await
        .unwrap();
        let history = session::load_history_through_sequence(
            node.as_ref(),
            session_id,
            TEST_AGENT_DID,
            None,
            lifecycle.background_completion_input_through_sequence(),
        )
        .await
        .unwrap();
        assert_eq!(
            history.len(),
            2,
            "successor input must stay out of this attempt"
        );
    }
    #[tokio::test]
    async fn claim_pins_configured_budget_and_reclaim_preserves_unlimited_or_zero() {
        for (index, configured, durable_override) in [
            (0, Some(50_u64), None),
            (1, None, None),
            (2, Some(1_u64), Some(0_i64)),
        ] {
            let node = test_node().await;
            let mut request = insert_pending_request(
                &node,
                &format!("budget-{index}"),
                &format!("budget-session-{index}"),
                "2026-01-01T00:00:00Z",
                None,
                "interactive",
            )
            .await;
            // Even an old envelope carrying a value cannot select first-claim
            // allowance: only the invoking runtime configuration may do that.
            request.max_total_tokens = Some(999);
            let mut first = RequestLifecycle::new_with_execution_binding(
                node.clone(),
                TEST_BEHAVIOR_ID,
                TEST_AGENT_DID,
                request,
                3600,
                ExecutionOrigin::Interactive,
                TEST_BACKEND_ID,
            );
            first.set_configured_max_total_tokens(configured);
            assert!(matches!(
                first.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            ));
            assert_eq!(
                first.request().max_total_tokens,
                configured.map(|value| value as i64)
            );
            let mut resumed_request = first.request().clone();
            let expected = durable_override.or(resumed_request.max_total_tokens);
            let budget_patch = durable_override
                .map(|limit| format!(", max_total_tokens: {limit}"))
                .unwrap_or_default();
            let mutation = format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending"{budget_patch} }}) {{ _docID }} }}"#,
                escape_graphql_string(&resumed_request.doc_id)
            );
            crate::config_client::ConfigAccess::write_local_response(
                &node,
                "test.requeue_budget",
                &mutation,
            )
            .await
            .unwrap();
            resumed_request.max_total_tokens = expected;
            let mut resumed = RequestLifecycle::new_with_execution_binding(
                node.clone(),
                TEST_BEHAVIOR_ID,
                TEST_AGENT_DID,
                resumed_request,
                3600,
                ExecutionOrigin::Interactive,
                TEST_BACKEND_ID,
            );
            resumed.set_configured_max_total_tokens(Some(10_000));
            assert!(matches!(
                resumed.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            ));
            assert_eq!(resumed.request().max_total_tokens, expected);
        }
    }
}
