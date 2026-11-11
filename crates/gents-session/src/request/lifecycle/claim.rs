use super::*;
use anyhow::Context;

pub(crate) struct DurableClaimReceipt {
    request: AgentRequest,
    request_commit_cid: String,
    deadline_at: chrono::DateTime<chrono::Utc>,
    background_completion_input_through_sequence: Option<u32>,
    valid_until_at_claim: Option<chrono::DateTime<chrono::Utc>>,
    execution_generation: String,
    lease_ms: u64,
    folded_selection: Vec<super::queue::FoldedInput>,
}

pub enum DurableClaimOutcome {
    Claimed(Box<DurableClaimReceipt>),
    NotClaimed(ClaimOutcome),
}

impl DurableClaimOutcome {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn was_claimed(&self) -> bool {
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

async fn reject_pending_admission_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    reason: &str,
    terminalized_at: &str,
) -> Result<bool> {
    let doc = escape_graphql_string(&request.doc_id);
    let node = escape_graphql_string(&request.node_did);
    let reason_gql = escape_graphql_string(reason);
    let time_gql = escape_graphql_string(terminalized_at);
    let mutation = format!(
        r#"mutation($terminal_output: JSON) {{
        update_AgentRequest(docID: "{doc}", filter: {{ _docID: {{ _eq: "{doc}" }},
            node_did: {{ _eq: "{node}" }}, lifecycle_state: {{ _eq: "{pending}" }} }},
            input: {{ lifecycle_state: "{failed}", failure_reason: "{reason_gql}",
                terminalized_at: "{time_gql}", terminal_redrive_attempts: 0,
                terminal_output: $terminal_output }}) {{ _docID request_id workspace_id workspace_owner_node_did }}
    }}"#,
        pending = RequestLifecycleState::Pending.as_str(),
        failed = RequestLifecycleState::Failed.as_str()
    );
    let response = txn
        .execute_with_variables(
            &mutation,
            &serde_json::json!({
                "terminal_output": gents_protocol::output::TerminalOutput::NoMessage
            }),
        )
        .await?;
    let updated = response
        .get("data")
        .and_then(|data| data.get("update_AgentRequest"))
        .is_some_and(response_has_documents);
    if updated {
        if let Some(rows) = response["data"]["update_AgentRequest"].as_array() {
            for row in rows {
                crate::request::workspace_binding::release_terminal_writer_binding(txn, row)
                    .await?;
            }
        }
        crate::request::hooks::points::request_terminal(
            txn,
            &request.node_did,
            &request.request_id,
            RequestLifecycleState::Failed,
            reason,
            terminalized_at,
        )
        .await?;
        super::materialize::project_refused_request_session_in_txn(txn, request, terminalized_at)
            .await?;
    }
    Ok(updated)
}

/// What `request.claim.gate` decided for a request and what core did about
/// it (Lean `Hooks.claimStep` before core's own claim).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimGate {
    Allowed,
    Queued,
    /// Core superseded the pending request.
    Retired,
    /// Core failed the pending request; `applied` is whether its row was still pending.
    Rejected {
        applied: bool,
    },
}

/// Runs `request.claim.gate` and performs the transition its combined verdict
/// names. A plugin names the outcome; the pending-terminal writes are core's.
pub async fn claim_gate_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    claimed_at: &str,
    client: bool,
) -> Result<ClaimGate> {
    use crate::request::hooks::claim::ClaimVerdict;
    let verdict = crate::request::hooks::points::claim_gate(
        txn,
        &crate::request::hooks::registry::ClaimCandidate {
            request: request.clone(),
            client,
            at: claimed_at.to_owned(),
        },
    )
    .await?;
    match verdict {
        ClaimVerdict::Allow => Ok(ClaimGate::Allowed),
        ClaimVerdict::LeaveQueued => Ok(ClaimGate::Queued),
        ClaimVerdict::Retire { reason, successor } => {
            let successor = successor.context("claim retirement names no superseding request")?;
            supersede_pending_in_txn(
                txn,
                &request.node_did,
                &request.doc_id,
                &request.request_id,
                &successor,
                &reason,
                claimed_at,
            )
            .await?;
            Ok(ClaimGate::Retired)
        }
        ClaimVerdict::Reject { reason } => Ok(ClaimGate::Rejected {
            applied: reject_pending_admission_in_txn(txn, request, &reason, claimed_at).await?,
        }),
    }
}

/// Core's pending-supersede path (Lean `Hooks.ClaimCore.retire`): supersedes
/// the request if it is still pending and stages `request.terminal.stage` for
/// it. Returns whether the row was still pending.
pub(crate) async fn supersede_pending_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    doc_id: &str,
    request_id: &str,
    successor: &crate::request::hooks::claim::Successor,
    reason: &str,
    at: &str,
) -> Result<bool> {
    let mutation = super::queue::supersede_pending_mutation(
        doc_id,
        node_did,
        &successor.request_id,
        &successor.request_doc_id,
        reason,
    );
    let response = txn.execute_local_response(&mutation).await?;
    let superseded = response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentRequest"))
        .is_some_and(response_has_documents);
    if superseded {
        crate::request::hooks::points::request_terminal(
            txn,
            node_did,
            request_id,
            RequestLifecycleState::Superseded,
            reason,
            at,
        )
        .await?;
    }
    Ok(superseded)
}

async fn claim_request_with_projection<F>(
    access: &gents_store::config_client::ConfigAccess,
    capture_background_snapshot: bool,
    request: &AgentRequest,
    claimed_at: &str,
    fold_admitted: &[String],
    client_claim_admission: Option<&crate::request::admission::ClientClaimAdmission>,
    build_mutation: F,
) -> Result<
    Option<
        std::result::Result<
            (
                defra_node::QueryResponse,
                BackgroundCompletionClaimSnapshot,
                Vec<super::queue::FoldedInput>,
            ),
            (),
        >,
    >,
>
where
    F: Fn(&str) -> String + Sync,
{
    let session_id = request.session_id.as_str();
    let scope = gents_protocol::session::session_scope_filter(
        &request.node_did,
        session_id,
        request.requester_did.as_deref(),
    );
    let snapshot_query = format!(
        r#"{{
            all_messages: AgentMessage(
                filter: {{ {scope} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
            notifications: AgentMessage(
                filter: {{
                    {scope},
                    request_doc_id: {{ _ne: null }},
                    message_key: {{ _like: "background-completion-notification:%" }}
                }},
                order: {{ sequence: ASC }}
            ) {{ sequence message_key request_doc_id }}
        }}"#
    );
    let snapshot_query = &snapshot_query;
    let build_mutation = &build_mutation;
    access.transact_write(
        "lifecycle.claim_request",
        move |txn| {
            Box::pin(async move {
                let claim_identity = txn.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ admission_kind }} }}"#,
                    escape_graphql_string(&request.doc_id),
                )).await?;
                let rows = claim_identity["data"]["AgentRequest"].as_array()
                    .context("claim admission identity query omitted rows")?;
                if rows.len() != 1 { return Ok(None); }
                let client = rows[0]["admission_kind"].as_str() == Some("client");
                match client_claim_admission {
                    Some(admission) => match admission.authorize_in_txn(txn, request).await? {
                        crate::request::admission::ClientClaimAuthorization::Allowed => {}
                        crate::request::admission::ClientClaimAuthorization::Unavailable => return Ok(None),
                        crate::request::admission::ClientClaimAuthorization::Denied => {
                            let rejected = reject_pending_admission_in_txn(
                                txn, request, "request authorization denied", claimed_at,
                            ).await?;
                            return Ok(rejected.then_some(Err(())));
                        }
                    },
                    None if client => return Ok(None),
                    None => {}
                }
                if crate::session::session_stopped_in_txn(
                    txn,
                    &request.node_did,
                    &request.session_id,
                    request.requester_did.as_deref(),
                )
                .await?
                {
                    super::queue::drain_all_pending_session_requests_in_txn(
                        txn,
                        &request.session_id,
                        &request.node_did,
                        request.requester_did.as_deref(),
                        "pending work drained because the session is stopped",
                    )
                    .await?;
                    return Ok(None);
                }
                match claim_gate_in_txn(txn, request, claimed_at, client).await? {
                    ClaimGate::Allowed => {}
                    ClaimGate::Queued | ClaimGate::Retired => return Ok(None),
                    ClaimGate::Rejected { applied } => return Ok(applied.then_some(Err(()))),
                }
                if request.purpose == gents_protocol::request_admission::RequestPurpose::Normal
                    && !super::query::claim_queue_allows(txn, request).await?
                {
                    return Ok(None);
                }
                let snapshot = if capture_background_snapshot {
                    let response = txn.execute_local_response(snapshot_query).await?;
                    let through_sequence = response
                        .data
                        .as_ref()
                        .and_then(|data| data.get("all_messages"))
                        .and_then(serde_json::Value::as_array)
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("sequence"))
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|sequence| u32::try_from(sequence).ok());
                    let notifications = || response.data.as_ref()
                        .and_then(|data| data.get("notifications"))
                        .and_then(serde_json::Value::as_array)
                        .into_iter().flatten()
                        .filter(|row| row.get("sequence").and_then(serde_json::Value::as_u64)
                            .zip(through_sequence.map(u64::from))
                            .is_some_and(|(sequence, cutoff)| sequence <= cutoff));
                    let asked = std::iter::once(request.doc_id.clone())
                        .chain(notifications().filter_map(|row| {
                            row.get("request_doc_id").and_then(serde_json::Value::as_str).map(str::to_owned)
                        }))
                        .collect();
                    let assignments = crate::request::hooks::points::queue_scope(
                        txn,
                        &request.node_did,
                        session_id,
                        asked,
                    )
                    .await?;
                    let assignment = assignments.scope(&request.doc_id)?;
                    let current = assignments.request(&request.doc_id)?;
                    let (retained_keys, retained_cutoff) = if let Some(parent_doc) = current.retry_parent_request_doc_id.as_deref() {
                        let parent = assignments.request(parent_doc)?;
                        anyhow::ensure!(parent.lifecycle_state == Some(RequestLifecycleState::Failed)
                            && parent.input.as_ref().is_some_and(super::is_background_completion_request)
                            && current.retry_parent_request.as_deref() == Some(parent.request_id.as_str())
                            && current.caused_by_parent_request_doc_id.as_deref() == Some(parent_doc),
                            "background retry does not name its physical failed wake");
                        gents_store::identity::request_receipt::verify_runtime_local_control_receipt(current,
                            &request.node_did, &parent.request_id,
                            request.requester_did.as_deref().unwrap_or(&request.agent_did))?;
                        use gents_protocol::document_query::{
                            fields::agent_request as fields, Filter, Query, Selection,
                        };
                        let receipt = txn.run(&Query::select(Selection::new()
                            .field(fields::BACKGROUND_COMPLETION_INPUT_THROUGH_SEQUENCE)
                            .field(fields::BACKGROUND_COMPLETION_NOTIFICATION_KEYS_JSON))
                            .filter(Filter::eq(fields::DOC_ID, parent_doc))
                            .render()).await?;
                        let receipt = receipt.pointer("/data/AgentRequest/0").context("failed wake receipt disappeared")?;
                        let retained = receipt["background_completion_notification_keys_json"].as_str()
                            .map(crate::request::lifecycle::notification_receipt_keys).transpose()?.unwrap_or_default();
                        let cutoff = receipt["background_completion_input_through_sequence"].as_u64();
                        anyhow::ensure!(retained.is_empty() || cutoff.zip(through_sequence.map(u64::from)).is_some_and(|(prior,current)| prior <= current),
                            "failed wake receipt has no available revision cutoff");
                        (retained, cutoff)
                    } else { (Vec::new(), None) };
                    let mut notification_keys = Vec::new();
                    for row in notifications() {
                        let source = row.get("request_doc_id").and_then(serde_json::Value::as_str)
                            .context("background notification has no physical request binding")?;
                        let key = row.get("message_key").and_then(serde_json::Value::as_str)
                            .context("background notification has no key")?;
                        let retained = retained_keys.iter().any(|prior| prior == key)
                            && row.get("sequence").and_then(serde_json::Value::as_u64)
                                .zip(retained_cutoff).is_some_and(|(sequence,cutoff)| sequence <= cutoff);
                        if assignments.scope(source)? == assignment || retained {
                            notification_keys.push(key.to_string());
                        }
                    }
                    anyhow::ensure!(retained_keys.iter().all(|key| notification_keys.contains(key)),
                        "failed wake retained notifications are missing from the current snapshot");
                    BackgroundCompletionClaimSnapshot {
                        through_sequence,
                        notification_keys,
                    }
                } else {
                    BackgroundCompletionClaimSnapshot::default()
                };
                let snapshot_fields = if capture_background_snapshot { snapshot.mutation_fields() } else { Default::default() };
                let mutation = build_mutation(&snapshot_fields);
                let claimed = txn.execute_local_response(&mutation).await?;
                let mut folded_selection = Vec::new();
                if request.purpose == gents_protocol::request_admission::RequestPurpose::Normal
                    && claimed
                        .data
                        .as_ref()
                        .and_then(|data| data.get("update_AgentRequest"))
                        .is_some_and(response_has_documents)
                {
                    crate::request::hooks::points::claim_applied(txn, request, claimed_at).await?;
                    crate::session::mailbox::claim_reply_in_txn(txn, request, claimed_at).await?;
                    super::materialize::apply_request_session_projection(txn, request, claimed_at)
                        .await?;
                    folded_selection = super::queue::select_fold_in_claim_txn(
                        txn,
                        request,
                        fold_admitted,
                        claimed_at,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(Some(Ok((claimed, snapshot, folded_selection))))
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
    node: &std::sync::Arc<EmbeddedNode>,
    actor: identity::Did,
    doc_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(
        Selection::new()
            .field(fields::REQUEST_ID)
            .field(fields::INTERRUPT_REQUESTED_AT)
            .field(fields::VALID_UNTIL),
    )
    .filter(Filter::eq(fields::DOC_ID, doc_id))
    .limit(1)
    .render();
    let response = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor)
        .read(&query)
        .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("interrupt and TTL query omitted AgentRequest rows")?;
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(
        rows.first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("AgentRequest {doc_id} not found"))?,
    )?;
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

    pub async fn claim_pending_durable_with_inputs<T, F>(
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
            fetch_interrupt_and_ttl(&self.node, self.actor_did()?, &self.request.doc_id).await?;
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
        let (now, generation) = claim_inputs();
        match self.persist_pending_claim_at(now, generation).await? {
            Some(receipt) => Ok(DurableClaimOutcome::Claimed(Box::new(receipt))),
            None => Ok(DurableClaimOutcome::NotClaimed(ClaimOutcome::Queued)),
        }
    }

    /// Commit the modeled claimed → processing boundary before allocating a
    /// local stream buffer. Every provider-capable path, including title
    /// dispatch and pre-inference compaction, must cross this fence before
    /// starting: terminalization uses an owned Claimed state as positive
    /// evidence of no provider work.
    pub async fn begin_owned_execution(
        &mut self,
        stream_writer: &crate::request::streaming::DefraStreamWriter,
    ) -> Result<()> {
        // Repeating an acknowledged begin is a read-only ownership check; it
        // must not reset buffered output on the live writer.
        if self.state == LocalLifecycleState::Streaming {
            return self.validate_owned_execution().await;
        }
        self.ensure_state(&[LocalLifecycleState::Claimed], "begin_owned_execution")?;
        let generation = self.execution_generation()?.to_owned();
        let request_doc_id = self.request.doc_id.clone();
        Self::begin_owned_execution_with_access(
            &self.access()?,
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

    #[cfg(test)]
    pub(crate) async fn begin_owned_execution_at_with_access<F>(
        access: &gents_store::config_client::ConfigAccess,
        request_doc_id: &str,
        generation: &str,
        now: F,
    ) -> Result<()>
    where
        F: Fn() -> chrono::DateTime<chrono::Utc> + Sync,
    {
        Self::begin_owned_execution_with_access(access, request_doc_id, generation, now).await
    }

    async fn begin_owned_execution_with_access<F>(
        access: &gents_store::config_client::ConfigAccess,
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
        access
            .transact_idempotent(
                gents_store::config_client::IdempotentTransactionRetry::Standard,
                "lifecycle.begin_owned_execution",
                move |txn| {
                    let generation = generation.clone();
                    let request_doc_id = request_doc_id.clone();
                    Box::pin(async move {
                        use gents_protocol::document_query::{
                            fields::agent_request as fields, Filter, Input, Query, Selection,
                            Update,
                        };
                        let read = Query::select(
                            Selection::new()
                                .field(fields::REQUEST_ID)
                                .field(fields::LIFECYCLE_STATE)
                                .field(fields::EXECUTION_GENERATION)
                                .field(fields::EXECUTION_LEASE_EXPIRES_AT),
                        )
                        .filter(Filter::eq(fields::DOC_ID, &request_doc_id))
                        .limit(2)
                        .render();
                        let response = txn.run(&read).await?;
                        let rows = response["data"]["AgentRequest"]
                            .as_array()
                            .context("execution begin omitted request rows")?;
                        anyhow::ensure!(
                            rows.len() == 1,
                            "execution begin request is missing or ambiguous"
                        );
                        let row: gents_protocol::row::AgentRequestRow =
                            serde_json::from_value(rows[0].clone())?;
                        let state = row
                            .lifecycle_state
                            .context("execution begin missing lifecycle")?;
                        let observed_generation = row
                            .execution_generation
                            .as_deref()
                            .context("execution begin missing generation")?;
                        let expiry = row
                            .execution_lease_expires_at
                            .as_deref()
                            .context("execution begin missing lease expiry")?;
                        let observed = super::execution_policy::LeaseObservation {
                            request: state,
                            generation: observed_generation,
                            deadline_ms: chrono::DateTime::parse_from_rfc3339(expiry)?
                                .timestamp_millis(),
                        };
                        let now = now().timestamp_millis();
                        // A lost acknowledgement must not reapply the transition
                        // or reset its lease; it may acknowledge the exact live
                        // generation's already committed processing state.
                        if state == RequestLifecycleState::Processing {
                            anyhow::ensure!(
                                super::execution_policy::authorize_producer_decision(
                                    observed,
                                    &generation,
                                    now
                                ),
                                "execution begin replay lost ownership"
                            );
                            return Ok(());
                        }
                        anyhow::ensure!(
                            super::execution_policy::authorize_begin(observed, &generation, now),
                            "execution begin requires a live claimed or resuming generation"
                        );
                        let update = Update::doc(
                            &request_doc_id,
                            Input::new().set(
                                fields::LIFECYCLE_STATE,
                                RequestLifecycleState::Processing.as_str(),
                            ),
                        )
                        .filter(Filter::eq(fields::DOC_ID, &request_doc_id))
                        .filter(Filter::eq(fields::LIFECYCLE_STATE, state.as_str()))
                        .filter(Filter::eq(fields::EXECUTION_GENERATION, &generation))
                        .filter(Filter::eq(fields::EXECUTION_LEASE_EXPIRES_AT, expiry))
                        .render();
                        let response = txn.run(&update).await?;
                        anyhow::ensure!(
                            response["data"]["update_AgentRequest"]
                                .as_array()
                                .is_some_and(|rows| rows.len() == 1),
                            "execution begin lost its request CAS"
                        );
                        Ok(())
                    })
                },
            )
            .await
    }

    pub async fn transition_pending_to_interrupted(&mut self, _interrupt_at: &str) -> Result<()> {
        let terminalized_at = chrono::Utc::now().to_rfc3339();
        let mutation = {
            use gents_protocol::document_query::{
                fields::agent_request as fields, Filter, Input, Selection, Update,
            };
            Update::doc(
                &self.request.doc_id,
                Input::new()
                    .set(
                        fields::LIFECYCLE_STATE,
                        RequestLifecycleState::Interrupted.as_str(),
                    )
                    .set(fields::TERMINALIZED_AT, terminalized_at.as_str())
                    .set(fields::TERMINAL_REDRIVE_ATTEMPTS, 0_i32),
            )
            .filter(Filter::eq(fields::DOC_ID, &self.request.doc_id))
            .filter(Filter::eq(fields::NODE_DID, &self.request.node_did))
            .filter(Filter::eq(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::Pending.as_str(),
            ))
            .returning(
                Selection::doc_id()
                    .field(fields::REQUEST_ID)
                    .field(fields::WORKSPACE_ID)
                    .field(fields::WORKSPACE_OWNER_NODE_DID),
            )
            .render()
        };
        let request = &self.request;
        let resp = self
            .access()?
            .transact_idempotent(
                gents_store::config_client::IdempotentTransactionRetry::Standard,
                "interrupt_before_claim",
                |txn| {
                    let mutation = &mutation;
                    let terminalized_at = &terminalized_at;
                    Box::pin(async move {
                        let response = txn.run_local_response(mutation).await?;
                        if let Some(rows) = response
                            .data
                            .as_ref()
                            .and_then(|data| data.get("update_AgentRequest"))
                            .and_then(serde_json::Value::as_array)
                            .filter(|rows| !rows.is_empty())
                        {
                            for row in rows {
                                crate::request::workspace_binding::release_terminal_writer_binding(
                                    txn, row,
                                )
                                .await?;
                            }
                            crate::request::hooks::points::request_terminal(
                                txn,
                                &request.node_did,
                                &request.request_id,
                                RequestLifecycleState::Interrupted,
                                "interrupted before claim",
                                terminalized_at,
                            )
                            .await?;
                        }
                        Ok::<_, anyhow::Error>(response)
                    })
                },
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

    pub async fn transition_pending_to_dead_stale(&mut self) -> Result<()> {
        let terminalized_at = chrono::Utc::now().to_rfc3339();
        let mutation = {
            use gents_protocol::document_query::{
                fields::agent_request as fields, Filter, Input, Selection, Update,
            };
            Update::doc(
                &self.request.doc_id,
                Input::new()
                    .set(
                        fields::LIFECYCLE_STATE,
                        RequestLifecycleState::Dead.as_str(),
                    )
                    .set(fields::FAILURE_REASON, "Stale")
                    .set(fields::TERMINALIZED_AT, terminalized_at.as_str())
                    .set(fields::TERMINAL_REDRIVE_ATTEMPTS, 0_i32),
            )
            .filter(Filter::eq(fields::DOC_ID, &self.request.doc_id))
            .filter(Filter::eq(fields::NODE_DID, &self.request.node_did))
            .filter(Filter::eq(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::Pending.as_str(),
            ))
            .returning(
                Selection::doc_id()
                    .field(fields::REQUEST_ID)
                    .field(fields::WORKSPACE_ID)
                    .field(fields::WORKSPACE_OWNER_NODE_DID),
            )
            .render()
        };
        let request = &self.request;
        let resp = self
            .access()?
            .transact_idempotent(
                gents_store::config_client::IdempotentTransactionRetry::Standard,
                "expire_stale",
                |txn| {
                    let mutation = &mutation;
                    let terminalized_at = &terminalized_at;
                    Box::pin(async move {
                        let response = txn.run_local_response(mutation).await?;
                        if let Some(rows) = response
                            .data
                            .as_ref()
                            .and_then(|data| data.get("update_AgentRequest"))
                            .and_then(serde_json::Value::as_array)
                            .filter(|rows| !rows.is_empty())
                        {
                            for row in rows {
                                crate::request::workspace_binding::release_terminal_writer_binding(
                                    txn, row,
                                )
                                .await?;
                            }
                            crate::request::hooks::points::request_terminal(
                                txn,
                                &request.node_did,
                                &request.request_id,
                                RequestLifecycleState::Dead,
                                "Stale",
                                terminalized_at,
                            )
                            .await?;
                        }
                        Ok::<_, anyhow::Error>(response)
                    })
                },
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
        let reason_text = reason.to_owned();
        let terminalized_at = chrono::Utc::now().to_rfc3339();
        let request = &self.request;
        let updated = self
            .access()?
            .transact_idempotent(
                gents_store::config_client::IdempotentTransactionRetry::Standard,
                "lifecycle.reject_admission",
                |txn| {
                    Box::pin(reject_pending_admission_in_txn(
                        txn,
                        request,
                        reason,
                        &terminalized_at,
                    ))
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
            DurableClaimOutcome::Claimed(receipt) => *receipt,
            DurableClaimOutcome::NotClaimed(outcome) => return Ok(outcome),
        };
        self.request = receipt.request;
        self.request_commit_cid = Some(receipt.request_commit_cid);
        self.state = LocalLifecycleState::Claimed;
        self.claimed_deadline_at = Some(receipt.deadline_at);
        self.background_completion_input_through_sequence =
            receipt.background_completion_input_through_sequence;
        self.valid_until_at_claim = receipt.valid_until_at_claim;
        self.folded_selection = receipt.folded_selection;
        self.execution_lease = Some(RequestExecutionLease::new(
            receipt.execution_generation.clone(),
            self.actor_did()?,
        ));
        self.renewal_task = Some(super::execution_renewal::RenewalTask::start(
            self.node.clone(),
            self.actor_did()?,
            self.agent_signer.clone(),
            self.request.doc_id.clone(),
            receipt.execution_generation,
            receipt.lease_ms,
        ));
        Ok(ClaimOutcome::Claimed)
    }

    async fn persist_pending_claim_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        execution_generation: String,
    ) -> Result<Option<DurableClaimReceipt>> {
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
        let synthesized_deadline_at = i64::try_from(self.deadline_duration_secs)
            .ok()
            .and_then(chrono::Duration::try_seconds)
            .and_then(|duration| now.checked_add_signed(duration))
            .context("request deadline duration is not representable")?;
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
        let request_fields = gents_protocol::row::AGENT_REQUEST_FIELDS;

        // The generation is written on every successful claim and never cleared
        // when the same physical request resumes; it distinguishes an unclaimed
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
            crate::request::aggregate_token_limit::parse_aggregate_token_limit(
                self.request.max_total_tokens,
            )?;
            String::new()
        };

        let build_mutation = |background_completion_snapshot_fields: &str| {
            format!(
                r#"mutation {{
                update_AgentRequest(
                    docID: "{escaped_doc_id}", filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        purpose: {{ _eq: "{escaped_purpose}" }},
                        lifecycle_state: {{ _eq: "{pending}" }},
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
                pending = RequestLifecycleState::Pending.as_str()
            )
        };

        let is_background_completion = self.request.purpose
            == gents_protocol::request_admission::RequestPurpose::Normal
            && crate::request::lifecycle::is_background_completion_request(&self.request.input);
        let Some(claim_result) = claim_request_with_projection(
            &self.access()?,
            is_background_completion,
            &self.request,
            &claimed_at,
            &self.fold_admitted,
            self.client_claim_admission.as_ref(),
            &build_mutation,
        )
        .await?
        else {
            return Ok(None);
        };
        let (resp, snapshot, folded_selection) =
            claim_result.map_err(|()| ClaimAdmissionError::AuthorizationDenied)?;
        let background_completion_input_through_sequence = snapshot.through_sequence;

        // The mutation response is the only response that can carry the exact
        // commit produced by this claim; a post-update query could read another
        // branchable write and falsely bind this lifecycle to a commit it did
        // not author.
        let claimed_request = crate::request::watcher::agent_request_from_mutation_response(
            &resp,
            "update_AgentRequest",
        )?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "request {doc_id} was not pending when this runtime attempted to claim it"
            )
        })?;
        let request_commit_cid =
            gents_store::graphql::mutation_composite_version(&resp, "update_AgentRequest")?
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

        Ok(Some(DurableClaimReceipt {
            request: claimed_request,
            request_commit_cid,
            deadline_at,
            background_completion_input_through_sequence,
            valid_until_at_claim: self.valid_until_at_claim,
            execution_generation,
            lease_ms: lease_ms as u64,
            folded_selection,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

}
