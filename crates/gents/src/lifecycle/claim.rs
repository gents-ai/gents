use super::*;
use anyhow::Context;

/// Durable claim receipt. Scheduling a renewal and retaining local ownership
/// are deliberately separate from the atomic request/session claim below.
pub(crate) struct DurableClaimReceipt {
    request: AgentRequest,
    request_commit_cid: String,
    deadline_at: chrono::DateTime<chrono::Utc>,
    background_completion_input_through_sequence: Option<u32>,
    valid_until_at_claim: Option<chrono::DateTime<chrono::Utc>>,
    execution_generation: String,
    lease_ms: u64,
}

pub(crate) enum DurableClaimOutcome {
    Claimed(DurableClaimReceipt),
    NotClaimed(ClaimOutcome),
}

impl DurableClaimOutcome {
    #[cfg(test)]
    pub(crate) fn was_claimed(&self) -> bool {
        matches!(self, Self::Claimed(_))
    }
}

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
                if request.purpose == gents_protocol::request_admission::RequestPurpose::Normal
                    && claimed
                        .data
                        .as_ref()
                        .and_then(|data| data.get("update_AgentRequest"))
                        .is_some_and(response_has_documents)
                {
                    crate::mailbox::claim_reply_in_txn(&txn, request, claimed_at).await?;
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
    pub async fn claim(&mut self) -> Result<ClaimOutcome> {
        self.claim_inner(false).await
    }

    pub async fn claim_with_identity(&mut self) -> Result<ClaimOutcome> {
        self.claim_inner(true).await
    }

    /// The one durable claim entry: pre-claim gates and the atomic claim /
    /// mailbox / session projection. The clock closures are invoked at the
    /// original observation boundaries, after their preceding async reads.
    pub(crate) async fn claim_pending_durable_with_inputs<T, F>(
        &mut self,
        ttl_now: T,
        claim_inputs: F,
    ) -> Result<DurableClaimOutcome>
    where
        T: FnOnce() -> chrono::DateTime<chrono::Utc>,
        F: FnOnce() -> (chrono::DateTime<chrono::Utc>, String),
    {
        self.ensure_state(&[LocalLifecycleState::Pending], "claim")?;
        let (interrupt_requested_at, valid_until) =
            fetch_interrupt_and_ttl(&self.node, &self.request.doc_id).await?;
        if let Some(interrupt_at) = interrupt_requested_at {
            self.transition_pending_to_interrupted(&interrupt_at)
                .await?;
            self.state = LocalLifecycleState::Interrupted;
            return Ok(DurableClaimOutcome::NotClaimed(ClaimOutcome::Interrupted));
        }
        self.valid_until_at_claim =
            match super::parse_valid_until(valid_until.as_deref(), ttl_now()) {
                super::TtlOutcome::Malformed(error) => {
                    anyhow::bail!(
                        "invalid valid_until on request {}: {error}",
                        self.request.doc_id
                    );
                }
                super::TtlOutcome::Expired(_) => {
                    self.transition_pending_to_dead_stale().await?;
                    self.state = LocalLifecycleState::Dead;
                    return Ok(DurableClaimOutcome::NotClaimed(ClaimOutcome::Expired));
                }
                super::TtlOutcome::NotSet => None,
                super::TtlOutcome::Live(parsed) => Some(parsed),
            };
        if self.request.purpose == gents_protocol::request_admission::RequestPurpose::Normal {
            let dedup = self.check_deduplication().await?;
            if !dedup.is_earliest {
                tracing::info!(
                    request_id = %self.request.request_id,
                    session_id = %self.request.session_id,
                    blocking_request_id = dedup.blocking_request_id.as_deref().unwrap_or(""),
                    "request remains queued behind earlier same-session request"
                );
                return Ok(DurableClaimOutcome::NotClaimed(ClaimOutcome::Queued));
            }
        }
        let (now, generation) = claim_inputs();
        Ok(DurableClaimOutcome::Claimed(
            self.persist_pending_claim_at(now, generation).await?,
        ))
    }

    /// Commit the modeled claimed → processing boundary before allocating a
    /// local stream buffer. Beginning creates no output document and does not
    /// renew the lease. A committed begin is the durable replay fact.
    pub async fn begin_owned_execution(
        &mut self,
        stream_writer: &crate::streaming::DefraStreamWriter,
    ) -> Result<()> {
        // Repeating an acknowledged begin is a read-only ownership check.
        // In particular it must not reset buffered output on the live writer.
        if self.state == LocalLifecycleState::Streaming {
            return self.validate_owned_execution().await;
        }
        self.ensure_state(&[LocalLifecycleState::Claimed], "begin_owned_execution")?;
        let generation = self.execution_generation()?.to_owned();
        let request_doc_id = self.request.doc_id.clone();
        Self::begin_owned_execution_durable_with_clock(
            &self.node,
            &request_doc_id,
            &generation,
            chrono::Utc::now,
        )
        .await?;
        stream_writer
            .initialize_request_buffer(&self.request.doc_id)
            .await?;
        self.state = LocalLifecycleState::Streaming;
        Ok(())
    }

    /// The single durable claimed-to-processing transaction. The live owner
    /// adds its process-local stream buffer after this commit; the native
    /// adapter exercises the same CAS with modeled time.
    pub(crate) async fn begin_owned_execution_durable_with_clock<F>(
        node: &Arc<EmbeddedNode>,
        request_doc_id: &str,
        generation: &str,
        now: F,
    ) -> Result<()>
    where
        F: Fn() -> chrono::DateTime<chrono::Utc> + Sync,
    {
        let generation = generation.to_owned();
        let request_doc_id = request_doc_id.to_owned();
        let now = &now;
        crate::config_client::ConfigAccess::transact_local_idempotent(
            node,
            None,
            crate::config_client::IdempotentTransactionRetry::Standard,
            "lifecycle.begin_owned_execution",
            move |txn| {
                let generation = generation.clone();
                let request_doc_id = request_doc_id.clone();
                Box::pin(async move {
                    let doc = escape_graphql_string(&request_doc_id);
                    let response = txn.execute(&format!(r#"{{ AgentRequest(
                        filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2
                    ) {{ request_id lifecycle_state execution_generation execution_lease_expires_at }} }}"#)).await?;
                    let rows = response["data"]["AgentRequest"].as_array()
                        .context("execution begin omitted request rows")?;
                    anyhow::ensure!(rows.len() == 1, "execution begin request is missing or ambiguous");
                    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
                    let state = row.lifecycle_state.context("execution begin missing lifecycle")?;
                    let observed_generation = row.execution_generation.as_deref()
                        .context("execution begin missing generation")?;
                    let expiry = row.execution_lease_expires_at.as_deref()
                        .context("execution begin missing lease expiry")?;
                    let observed = super::execution_policy::LeaseObservation {
                        request: state,
                        generation: observed_generation,
                        deadline_ms: chrono::DateTime::parse_from_rfc3339(expiry)?.timestamp_millis(),
                    };
                    let now = now().timestamp_millis();
                    // A lost acknowledgement must not reapply the transition
                    // or reset its lease. It may acknowledge the exact live
                    // generation's already committed processing state.
                    if state == RequestLifecycleState::Processing {
                        anyhow::ensure!(super::execution_policy::authorize_producer_decision(
                            observed, &generation, now), "execution begin replay lost ownership");
                        return Ok(());
                    }
                    anyhow::ensure!(super::execution_policy::authorize_begin(observed, &generation, now),
                        "execution begin requires a live claimed generation");
                    let response = txn.execute(&format!(r#"mutation {{ update_AgentRequest(
                        filter: {{ _docID: {{ _eq: "{doc}" }}, lifecycle_state: {{ _eq: "claimed" }},
                            execution_generation: {{ _eq: "{}" }}, execution_lease_expires_at: {{ _eq: "{}" }} }},
                        input: {{ lifecycle_state: "processing" }}
                    ) {{ _docID }} }}"#, escape_graphql_string(&generation), escape_graphql_string(expiry))).await?;
                    anyhow::ensure!(response["data"]["update_AgentRequest"].as_array()
                        .is_some_and(|rows| rows.len() == 1), "execution begin lost its request CAS");
                    Ok(())
                })
            },
        ).await
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
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let reason_text = reason.to_string();
        let reason = escape_graphql_string(&reason_text);
        let terminalized_at_value = chrono::Utc::now().to_rfc3339();
        let terminalized_at = escape_graphql_string(&terminalized_at_value);
        let request_mutation = format!(
            r#"mutation($terminal_output: JSON) {{
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
                        terminal_redrive_attempts: 0,
                        terminal_output: $terminal_output
                    }}
                ) {{ _docID }}
            }}"#
        );
        let request_mutation = &request_mutation;
        let updated =
            crate::config_client::ConfigAccess::transact_local_idempotent(
                &self.node,
                None,
                crate::config_client::IdempotentTransactionRetry::Standard,
                "lifecycle.reject_admission",
                move |txn| {
                    Box::pin(async move {
                        let response = txn.execute_with_variables(
                        request_mutation,
                        &serde_json::json!({
                            "terminal_output": gents_protocol::output::TerminalOutput::NoMessage
                        }),
                    ).await?;
                        let updated = response
                            .get("data")
                            .and_then(|data| data.get("update_AgentRequest"))
                            .is_some_and(response_has_documents);
                        Ok::<_, anyhow::Error>(updated)
                    })
                },
            )
            .await?;

        if !updated {
            let request_view = self.request_view().await?;
            if !request_view.as_ref().is_some_and(|row| {
                row.lifecycle_state == Some(RequestLifecycleState::Failed)
                    && row.failure_reason.as_deref() == Some(reason_text.as_str())
                    && row.terminal_output
                        == Some(gents_protocol::output::TerminalOutput::NoMessage)
            }) {
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
        self.failure_reason = Some(reason_text);
        self.state = LocalLifecycleState::Failed;
        Ok(())
    }

    async fn claim_inner(&mut self, _explicit_did: bool) -> Result<ClaimOutcome> {
        let receipt = match self
            .claim_pending_durable_with_inputs(chrono::Utc::now, || {
                (chrono::Utc::now(), uuid::Uuid::new_v4().to_string())
            })
            .await?
        {
            DurableClaimOutcome::Claimed(receipt) => receipt,
            DurableClaimOutcome::NotClaimed(outcome) => return Ok(outcome),
        };
        self.request = receipt.request;
        self.request_commit_cid = Some(receipt.request_commit_cid);
        self.state = LocalLifecycleState::Claimed;
        self.claimed_deadline_at = Some(receipt.deadline_at);
        self.background_completion_input_through_sequence =
            receipt.background_completion_input_through_sequence;
        self.valid_until_at_claim = receipt.valid_until_at_claim;
        self.execution_lease = Some(RequestExecutionLease::new(
            receipt.execution_generation.clone(),
        ));
        self.renewal_task = Some(super::execution_renewal::RenewalTask::start(
            self.node.clone(),
            self.request.doc_id.clone(),
            receipt.execution_generation,
            receipt.lease_ms,
        ));
        Ok(ClaimOutcome::Claimed)
    }

    /// Persist the exact pending-to-claimed transaction without arming the
    /// process-local renewal scheduler. The native conformance adapter supplies
    /// model time/generation here; production supplies wall time and a fresh UUID.
    async fn persist_pending_claim_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        execution_generation: String,
    ) -> Result<DurableClaimReceipt> {
        self.ensure_state(&[LocalLifecycleState::Pending], "claim")?;
        let claimed_at = now.to_rfc3339();
        let lease_secs = i64::try_from(self.execution_lease_duration_secs)
            .context("execution lease duration out of range")?;
        let lease_ms = lease_secs
            .checked_mul(1000)
            .filter(|value| *value > 0)
            .context("execution lease duration must be positive and representable")?;
        let execution_lease_expires_at = now
            .checked_add_signed(chrono::Duration::milliseconds(lease_ms))
            .context("execution lease expiry out of range")?;
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
        let escaped_purpose = escape_graphql_string(self.request.purpose.as_str());
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
                        purpose: {{ _eq: "{escaped_purpose}" }},
                        lifecycle_state: {{ _eq: "pending" }},
                        execution_generation: {{ _eq: {prior_generation} }}
                    }},
                    input: {{
                        lifecycle_state: "{lifecycle_state}",
                        backend_id: "{escaped_backend_id}",
                        claimed_at: "{escaped_claimed_at}",
                        execution_generation: "{escaped_execution_generation}",
                        execution_lease_expires_at: "{escaped_execution_lease_expires_at}",
                        execution_lease_secs: {lease_secs},
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

        let is_background_completion = self.request.purpose
            == gents_protocol::request_admission::RequestPurpose::Normal
            && crate::lifecycle::is_background_completion_request(&self.request.input);
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
        tracing::debug!(
            doc_id = %doc_id,
            deadline = %deadline,
            backend_id = %self.backend_id,
            execution_origin,
            "claimed agent request with exact DefraDB version"
        );

        Ok(DurableClaimReceipt {
            request: claimed_request,
            request_commit_cid,
            deadline_at,
            background_completion_input_through_sequence,
            valid_until_at_claim: self.valid_until_at_claim,
            execution_generation,
            lease_ms: lease_ms as u64,
        })
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
                    purpose: "normal",
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

    async fn durable_request_row(
        node: &EmbeddedNode,
        doc_id: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let doc_id = escape_graphql_string(doc_id);
        let response = node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        crate::graphql::first_row::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn durable_claim_and_begin_use_real_owners_without_arming_local_renewal() {
        let node = test_node().await;
        let request = insert_pending_request(
            &node,
            "durable-claim",
            "durable-session",
            "2023-11-14T22:13:25Z",
            None,
            "interactive",
        )
        .await;
        let doc_id = request.doc_id.clone();
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            request,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        lifecycle.set_execution_lease_duration(std::time::Duration::from_secs(5));
        let now = chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:25Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let outcome = lifecycle
            .claim_pending_durable_with_inputs(|| now, || (now, "lean-generation-7".into()))
            .await
            .unwrap();
        let DurableClaimOutcome::Claimed(receipt) = outcome else {
            panic!("fresh request was not durably claimed")
        };
        assert_eq!(receipt.request.doc_id, doc_id);
        assert_eq!(receipt.execution_generation, "lean-generation-7");
        assert_eq!(receipt.lease_ms, 5_000);
        assert_eq!(lifecycle.state, LocalLifecycleState::Pending);
        assert!(lifecycle.execution_lease.is_none());
        assert!(lifecycle.renewal_task.is_none());

        let claimed = durable_request_row(&node, &doc_id).await;
        assert_eq!(
            claimed.lifecycle_state,
            Some(RequestLifecycleState::Claimed)
        );
        assert_eq!(
            claimed.execution_generation.as_deref(),
            Some("lean-generation-7")
        );
        assert_eq!(
            claimed.execution_lease_expires_at.as_deref(),
            Some("2023-11-14T22:13:30+00:00")
        );
        RequestLifecycle::begin_owned_execution_durable_with_clock(
            &node,
            &doc_id,
            "lean-generation-7",
            || now,
        )
        .await
        .unwrap();
        RequestLifecycle::begin_owned_execution_durable_with_clock(
            &node,
            &doc_id,
            "lean-generation-7",
            || now + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
        assert_eq!(
            durable_request_row(&node, &doc_id).await.lifecycle_state,
            Some(RequestLifecycleState::Processing)
        );
        assert!(
            RequestLifecycle::begin_owned_execution_durable_with_clock(
                &node,
                &doc_id,
                "lean-generation-7",
                || now + chrono::Duration::seconds(6),
            )
            .await
            .is_err(),
            "expired replay must not acknowledge ownership"
        );
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
        // The claim owner observes already-published canonical history; this
        // fixture tests snapshot isolation, not notification authorization.
        session::import_history_observation(
            &node,
            "prior-request-doc",
            session_id,
            TEST_AGENT_DID,
            None,
            "first notification",
            "background-completion-notification:child-1:subagent",
            1,
            None,
        )
        .await;
        session::import_history_observation(
            &node,
            "prior-request-doc",
            session_id,
            TEST_AGENT_DID,
            None,
            "prior context",
            "prior-context",
            2,
            None,
        )
        .await;

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

        session::import_history_observation(
            &node,
            "successor-request-doc",
            session_id,
            TEST_AGENT_DID,
            None,
            "successor notification",
            "background-completion-notification:child-2:subagent",
            3,
            None,
        )
        .await;
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
