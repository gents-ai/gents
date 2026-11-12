use chrono::{DateTime, Duration, Utc};

/// Mirrors `RequestBackendWake.fallback`; a retry never extends the request's
/// original execution deadline, even when the runtime is restarted.
pub(crate) fn fallback_wake(
    now: DateTime<Utc>,
    deadline: DateTime<Utc>,
    previous_attempts: u32,
) -> Option<DateTime<Utc>> {
    if now >= deadline {
        return None;
    }
    let exponent = previous_attempts.min(5);
    let delay_ms = (1_000_i64 * (1_i64 << exponent)).min(30_000);
    Some(
        now.checked_add_signed(Duration::milliseconds(delay_ms))
            .unwrap_or(deadline)
            .min(deadline),
    )
}

pub(crate) fn fresh_healthy(
    wait_started: DateTime<Utc>,
    now: DateTime<Utc>,
    observed: Option<DateTime<Utc>>,
) -> bool {
    observed.is_some_and(|time| time >= wait_started && time <= now)
}

/// Mirrors `RequestBackendWake.resumedTurn`: the turn after every provider
/// turn the request completed before it parked.
pub(crate) fn resumed_turn_index(completed: impl IntoIterator<Item = u32>) -> usize {
    completed
        .into_iter()
        .max()
        .map_or(0, |last| last as usize + 1)
}

#[derive(Debug, serde::Deserialize)]
struct BackendWaitMetadata {
    #[serde(rename = "_docID")]
    doc_id: String,
    node_did: String,
    claim_cid: String,
    attempts: i64,
    retry_at: String,
    wait_started_at: String,
}

async fn load_wait_metadata(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
) -> anyhow::Result<Option<BackendWaitMetadata>> {
    let doc = gents_store::graphql::escape_graphql_string(request_doc_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequestBackendWait(filter: {{ request_doc_id: {{ _eq: "{doc}" }} }}, limit: 1) {{
                _docID node_did claim_cid attempts retry_at wait_started_at
            }} }}"#
        ))
        .await?;
    Ok(gents_store::graphql::first_row(
        &response,
        "AgentRequestBackendWait",
    )?)
}

fn can_repark_expired<T>(
    resuming: bool,
    observed_generation: &T,
    expected_generation: &T,
    observed_expiry_ms: i64,
    expected_expiry_ms: i64,
    now_ms: i64,
) -> bool
where
    T: PartialEq,
{
    resuming
        && observed_generation == expected_generation
        && observed_expiry_ms == expected_expiry_ms
        && observed_expiry_ms <= now_ms
}

pub async fn repark_expired_resume(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    generation: &str,
    expiry: &str,
) -> anyhow::Result<bool> {
    use gents_store::config_client::IdempotentTransactionRetry;
    let doc = request_doc_id.to_owned();
    let generation = generation.to_owned();
    let expiry = expiry.to_owned();
    access.transact_idempotent(IdempotentTransactionRetry::Standard,
        "lifecycle.repark_expired_backend_resume", move |txn| {
            let doc = doc.clone(); let generation = generation.clone(); let expiry = expiry.clone();
            Box::pin(async move {
                use anyhow::Context;
                use gents_store::graphql::{escape_graphql_string, response_has_documents};
                use gents_protocol::request_lifecycle::RequestLifecycleState as State;
                let request_doc_id = doc.clone();
                let doc = escape_graphql_string(&doc);
                let result = txn.execute_local_response(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 1) {{ request_id node_did lifecycle_state execution_generation execution_lease_expires_at }} }}"#)).await?;
                let row = gents_store::graphql::first_row::<gents_protocol::row::AgentRequestRow>(&result, "AgentRequest")?.context("resuming request disappeared")?;
                if row.lifecycle_state == Some(State::WaitingForBackend) && row.execution_generation.as_deref() == Some(generation.as_str()) { return Ok(true); }
                let Some(observed_generation) = row.execution_generation.as_deref() else { return Ok(false) };
                let Some(observed_expiry) = row.execution_lease_expires_at.as_deref() else { return Ok(false) };
                let observed_ms = DateTime::parse_from_rfc3339(observed_expiry)?.timestamp_millis();
                let expected_ms = DateTime::parse_from_rfc3339(&expiry)?.timestamp_millis();
                let now = Utc::now();
                if !can_repark_expired(row.lifecycle_state == Some(State::ResumingBackend), &observed_generation, &generation.as_str(), observed_ms, expected_ms, now.timestamp_millis()) || observed_expiry != expiry || DateTime::parse_from_rfc3339(observed_expiry)?.with_timezone(&Utc) > now { return Ok(false) }
                let generation = escape_graphql_string(&generation); let expiry = escape_graphql_string(&expiry);
                let retry_at = escape_graphql_string(&now.to_rfc3339());
                let wait = load_wait_metadata(txn, &request_doc_id).await?.context("resuming backend wait metadata is missing")?;
                anyhow::ensure!(row.node_did.as_deref() == Some(wait.node_did.as_str()), "backend wait belongs to a different node");
                let result = txn.execute_local_response(&format!(r#"mutation {{ update_AgentRequest(docID: "{doc}", filter: {{ _docID: {{ _eq: "{doc}" }}, lifecycle_state: {{ _eq: "resumingBackend" }}, execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }}, input: {{ lifecycle_state: "waitingForBackend", execution_lease_expires_at: null }} ) {{ _docID }} }}"#)).await?;
                if !result.data.as_ref().and_then(|data| data.get("update_AgentRequest")).is_some_and(response_has_documents) { return Ok(false); }
                let wait_doc = escape_graphql_string(&wait.doc_id);
                let result = txn.execute_local_response(&format!(r#"mutation {{ update_AgentRequestBackendWait(docID: "{wait_doc}", input: {{ retry_at: "{retry_at}" }} ) {{ _docID }} }}"#)).await?;
                anyhow::ensure!(result.data.as_ref().and_then(|data| data.get("update_AgentRequestBackendWait")).is_some_and(response_has_documents), "backend wait metadata update returned no document");
                Ok(true)
            })
        }).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkResult {
    Parked,
    AlreadyParked,
    Lost,
}

impl super::RequestLifecycle {
    pub async fn is_waiting_for_backend(&self) -> anyhow::Result<bool> {
        let row = self.request_view().await?;
        Ok(row.is_some_and(|row| {
            row.lifecycle_state
                == Some(gents_protocol::request_lifecycle::RequestLifecycleState::WaitingForBackend)
                && row.execution_generation == self.request.execution_generation
        }))
    }
    /// The turn a resumed request continues from, read from its closed
    /// inference sources: history may be compacted, and a rendered capture is
    /// optional, so neither counts the turns already spent.
    pub async fn resumed_turn_index(&self) -> anyhow::Result<usize> {
        use gents_protocol::output::{OutputOutcome, OutputSource, SourceClose};
        use gents_protocol::rendered_request::CaptureScopeKind;
        let request = &self.request;
        let records = self
            .access()?
            .transact_readonly("lifecycle.resumed_turn_index", move |txn| {
                Box::pin(async move {
                    super::recovery::request_segments(
                        txn,
                        &request.doc_id,
                        &request.node_did,
                        &request.session_id,
                        request.requester_did.as_deref(),
                    )
                    .await
                })
            })
            .await?;
        Ok(resumed_turn_index(records.iter().filter_map(
            |record| match (&record.segment.source, &record.segment.close) {
                (
                    OutputSource::ProviderTurn {
                        scope, turn_index, ..
                    },
                    Some(SourceClose::Closed {
                        outcome: OutputOutcome::Complete,
                        ..
                    }),
                ) if scope.kind == CaptureScopeKind::Inference => Some(*turn_index),
                _ => None,
            },
        )))
    }

    /// Release local ownership only after the durable parking transaction wins.
    pub async fn park_for_backend(&mut self) -> anyhow::Result<bool> {
        self.ensure_state(&[super::LocalLifecycleState::Streaming], "park_for_backend")?;
        let generation = self.execution_generation()?.to_owned();
        let claim_cid = self
            .request_commit_cid()
            .ok_or_else(|| anyhow::anyhow!("backend wait requires the exact claim version"))?
            .to_owned();
        let result = park_owned_execution(
            &self.access()?,
            &self.request.doc_id,
            &generation,
            &claim_cid,
            Utc::now(),
        )
        .await?;
        if matches!(result, ParkResult::Parked | ParkResult::AlreadyParked) {
            self.renewal_task.take();
            self.execution_lease.take();
            self.state = super::LocalLifecycleState::Parked;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Called only after a worker slot and the request's admission are secured.
    pub async fn wake_for_backend(
        &mut self,
        healthy_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<BackendWake> {
        use anyhow::Context;
        self.ensure_state(&[super::LocalLifecycleState::Pending], "wake_for_backend")?;
        let expected_deadline = DateTime::parse_from_rfc3339(
            self.request
                .deadline
                .as_deref()
                .context("parked request has no deadline")?,
        )?
        .with_timezone(&Utc);
        let result = wake_parked_execution(
            &self.access()?,
            &self.request.doc_id,
            &self.backend_id,
            healthy_at,
            Utc::now(),
        )
        .await?;
        let (generation, claim_cid, deadline, lease_ms, background_cutoff) = match result {
            WakeResult::Resumed {
                generation,
                claim_cid,
                deadline,
                lease_ms,
                background_cutoff,
            } => (generation, claim_cid, deadline, lease_ms, background_cutoff),
            WakeResult::NotDue => return Ok(BackendWake::NotDue),
            WakeResult::OtherBackend { bound } => return Ok(BackendWake::OtherBackend { bound }),
            WakeResult::Lost => return Ok(BackendWake::Lost),
        };
        self.execution_lease = Some(super::RequestExecutionLease::new(
            generation.clone(),
            self.actor_did()?,
        ));
        self.state = super::LocalLifecycleState::Claimed;
        anyhow::ensure!(
            deadline == expected_deadline,
            "parked request deadline changed before wake"
        );
        self.request.execution_generation = Some(generation.clone());
        self.claimed_deadline_at = Some(deadline);
        self.request_commit_cid = Some(claim_cid);
        self.background_completion_input_through_sequence = background_cutoff;
        self.folded_selection.clear();
        self.resumed_from_backend = true;
        self.renewal_task = Some(super::execution_renewal::RenewalTask::start(
            self.node.clone(),
            self.actor_did()?,
            self.agent_signer.clone(),
            self.request.doc_id.clone(),
            generation,
            lease_ms,
        ));
        Ok(BackendWake::Resumed)
    }
}

/// What a wake attempt did to a parked request, for the caller's account of
/// why it holds no claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendWake {
    Resumed,
    NotDue,
    OtherBackend { bound: String },
    Lost,
}

impl BackendWake {
    pub fn resumed(&self) -> bool {
        matches!(self, Self::Resumed)
    }

    pub fn bound_backend(&self) -> Option<&str> {
        match self {
            Self::OtherBackend { bound } => Some(bound),
            _ => None,
        }
    }

    pub fn unclaimed_reason(&self) -> &'static str {
        match self {
            Self::Resumed => "parked request resumed",
            Self::NotDue => {
                "parked request still waiting: its backend is not observed healthy and its retry is not due"
            }
            Self::OtherBackend { .. } => {
                "parked request still waiting: it stays bound to the backend it was claimed on, which is no longer this agent's backend"
            }
            Self::Lost => "parked request not resumed: it was interrupted, settled or resumed elsewhere",
        }
    }
}

/// The request CAS is permitted only after every provider source owned by this
/// generation is closed and no foreground tool remains in flight.
pub async fn park_owned_execution(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    generation: &str,
    claim_cid: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<ParkResult> {
    use gents_store::config_client::IdempotentTransactionRetry;
    let request_doc_id = request_doc_id.to_owned();
    let generation = generation.to_owned();
    let claim_cid = claim_cid.to_owned();
    access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "lifecycle.park_backend_wait",
            move |txn| {
                let request_doc_id = request_doc_id.clone();
                let generation = generation.clone();
                let claim_cid = claim_cid.clone();
                Box::pin(async move {
                    park_in_txn(
                        txn,
                        &request_doc_id,
                        &generation,
                        &claim_cid,
                        Utc::now().max(now),
                    )
                    .await
                })
            },
        )
        .await
}

async fn park_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    generation: &str,
    claim_cid: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<ParkResult> {
    use anyhow::Context;
    use gents_protocol::output::{OutputWriter, SourceClose};
    use gents_protocol::request_lifecycle::RequestLifecycleState as State;
    use gents_protocol::row::AgentRequestRow;
    use gents_store::graphql::{escape_graphql_string, response_has_documents};

    let doc_id = escape_graphql_string(request_doc_id);
    let response = txn.execute_local_response(&format!(r#"{{ AgentRequest(
        filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
        _docID request_id purpose node_did requester_did session_id lifecycle_state backend_id
        deadline execution_generation execution_lease_expires_at interrupt_requested_at terminal_output
    }} }}"#)).await?;
    let row = gents_store::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
        .context("backend wait request disappeared")?;
    let metadata = load_wait_metadata(txn, request_doc_id).await?;
    if row.lifecycle_state == Some(State::WaitingForBackend)
        && row.execution_generation.as_deref() == Some(generation)
        && metadata.as_ref().is_some_and(|wait| {
            wait.claim_cid == claim_cid && row.node_did.as_deref() == Some(wait.node_did.as_str())
        })
    {
        return Ok(ParkResult::AlreadyParked);
    }
    if row.lifecycle_state != Some(State::Processing)
        || row.execution_generation.as_deref() != Some(generation)
        || row
            .interrupt_requested_at
            .as_deref()
            .is_some_and(|v| !v.is_empty())
        || row.terminal_output.is_some()
    {
        return Ok(ParkResult::Lost);
    }
    let lease = row
        .execution_lease_expires_at
        .as_deref()
        .context("missing execution lease")?;
    if DateTime::parse_from_rfc3339(lease)?.with_timezone(&Utc) <= now {
        return Ok(ParkResult::Lost);
    }
    let deadline = DateTime::parse_from_rfc3339(
        row.deadline
            .as_deref()
            .context("missing request deadline")?,
    )?
    .with_timezone(&Utc);
    anyhow::ensure!(
        !claim_cid.trim().is_empty(),
        "backend wait requires a claim version"
    );
    anyhow::ensure!(
        metadata
            .as_ref()
            .is_none_or(|wait| wait.claim_cid == claim_cid),
        "backend wait cannot replace its original claim version"
    );
    let attempts = u32::try_from(metadata.as_ref().map_or(0, |wait| wait.attempts))?;
    let Some(wake) = fallback_wake(now, deadline, attempts) else {
        return Ok(ParkResult::Lost);
    };
    let next_attempt = attempts
        .checked_add(1)
        .context("backend retry count exhausted")?;
    let agent = row
        .node_did
        .as_deref()
        .context("request missing node identity")?;
    anyhow::ensure!(
        metadata.as_ref().is_none_or(|wait| wait.node_did == agent),
        "backend wait belongs to a different node"
    );
    let session = row
        .session_id
        .as_deref()
        .context("request missing session")?;
    let records = super::recovery::request_segments(
        txn,
        request_doc_id,
        agent,
        session,
        row.requester_did.as_deref(),
    )
    .await?;
    let headers = crate::request::streaming::canonical::request_headers_in_txn(
        txn,
        session,
        agent,
        row.requester_did.as_deref(),
        request_doc_id,
    )
    .await?;
    let writer = OutputWriter::RequestExecution {
        execution_generation: generation.to_owned(),
    };
    let mut sources = Vec::new();
    for record in records
        .iter()
        .filter(|record| record.segment.writer == writer)
    {
        if !sources.contains(&record.segment.source) {
            sources.push(record.segment.source.clone());
        }
    }
    let auxiliary = row
        .purpose
        .context("request missing purpose")?
        .is_auxiliary();
    for source in sources {
        let closings: Vec<_> = records
            .iter()
            .filter(|record| record.segment.source == source && record.segment.close.is_some())
            .collect();
        anyhow::ensure!(
            closings.len() == 1,
            "backend wait requires one closed provider source"
        );
        let closing = closings[0];
        anyhow::ensure!(
            closing.segment.writer == writer,
            "backend wait source closure has a different writer"
        );
        match closing.segment.close.as_ref().expect("filtered closure") {
            SourceClose::Retracted => {
                crate::request::streaming::canonical::validate_source_physical_identities(
                    &records,
                    request_doc_id,
                    &source,
                )?
            }
            SourceClose::Closed { outcome, .. } => {
                crate::request::streaming::canonical::validate_closed_source_extent(
                    &records,
                    request_doc_id,
                    &source,
                    closing,
                )?;
                // The publication owner writes a header for a Partial closure
                // only when it retains blocks; an attempt that failed before
                // any retained output closes Partial with nothing to publish.
                let publishable = *outcome != gents_protocol::output::OutputOutcome::Partial
                    || !crate::request::streaming::canonical::retained_partial_blocks(
                        &records
                            .iter()
                            .map(
                                |row| gents_protocol::output::reconstruction::ObservedSegment {
                                    doc_id: &row.doc_id,
                                    segment: &row.segment,
                                },
                            )
                            .collect::<Vec<_>>(),
                        request_doc_id,
                        &source,
                        &writer,
                        closing,
                    )?
                    .is_empty();
                if !auxiliary && publishable {
                    anyhow::ensure!(
                        headers.iter().any(|header| header
                            .message
                            .payload_references()
                            .iter()
                            .any(|reference| reference.close_doc_id == closing.doc_id)),
                        "closed provider output has not been published"
                    );
                }
            }
        }
    }
    let tools = txn.execute_local_response(&format!(r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }},
        lifecycle_state: {{ _in: ["pending", "running"] }},
        _or: [{{ await_mode: {{ _eq: "foreground" }} }}, {{ await_mode: {{ _eq: null }} }}] }}, limit: 1) {{ _docID }} }}"#)).await?;
    let pending = tools
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .context("foreground tool query omitted rows")?;
    anyhow::ensure!(
        pending.is_empty(),
        "cannot park while a foreground tool is active"
    );
    anyhow::ensure!(
        row.backend_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty()),
        "backend wait requires a selected backend"
    );
    let interrupt_filter = match row.interrupt_requested_at.as_deref() {
        Some(value) => format!("\"{}\"", escape_graphql_string(value)),
        None => "null".to_string(),
    };
    let generation = escape_graphql_string(generation);
    let expiry = escape_graphql_string(lease);
    let retry_at = escape_graphql_string(&wake.to_rfc3339());
    let wait_started_at = escape_graphql_string(&now.to_rfc3339());
    let claim_cid = escape_graphql_string(claim_cid);
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(docID: "{doc_id}",
        filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "processing" }},
        execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }},
        interrupt_requested_at: {{ _eq: {interrupt_filter} }} }},
        input: {{ lifecycle_state: "waitingForBackend", execution_lease_expires_at: null }} ) {{ _docID }} }}"#
    );
    let result = txn.execute_local_response(&mutation).await?;
    if !result
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentRequest"))
        .is_some_and(response_has_documents)
    {
        return Ok(ParkResult::Lost);
    }
    let mutation = if let Some(wait) = metadata {
        let wait_doc = escape_graphql_string(&wait.doc_id);
        format!(
            r#"mutation {{ update_AgentRequestBackendWait(docID: "{wait_doc}", input: {{ attempts: {next_attempt}, retry_at: "{retry_at}", wait_started_at: "{wait_started_at}" }}) {{ _docID }} }}"#
        )
    } else {
        let agent = escape_graphql_string(agent);
        format!(
            r#"mutation {{ create_AgentRequestBackendWait(input: {{ request_doc_id: "{doc_id}", node_did: "{agent}", claim_cid: "{claim_cid}", attempts: {next_attempt}, retry_at: "{retry_at}", wait_started_at: "{wait_started_at}" }}) {{ _docID }} }}"#
        )
    };
    let response = txn.execute_local_response(&mutation).await?;
    let field = if mutation.contains("create_AgentRequestBackendWait") {
        "create_AgentRequestBackendWait"
    } else {
        "update_AgentRequestBackendWait"
    };
    anyhow::ensure!(
        gents_store::graphql::single_mutation_document(&response, field)?.is_some(),
        "backend wait metadata write returned no document"
    );
    Ok(ParkResult::Parked)
}

#[cfg(test)]
mod tests {
    use super::{can_repark_expired, fallback_wake, fresh_healthy};
    use chrono::{TimeZone, Utc};

    #[test]
    fn fallback_matches_the_generated_model() {
        let at = |millis: u64| {
            Utc.timestamp_millis_opt(i64::try_from(millis).unwrap())
                .single()
                .unwrap()
        };
        gents_lean_vocab::families::backend_wake_fallback_wake::replay(|i| {
            fallback_wake(
                at(i.now_ms),
                at(i.deadline_ms),
                u32::try_from(i.attempts).unwrap(),
            )
            .map(|wake| u64::try_from(wake.timestamp_millis()).unwrap())
        });
    }

    #[test]
    fn expired_resume_reparking_matches_the_generated_model() {
        let millis = |value: u64| i64::try_from(value).unwrap();
        gents_lean_vocab::families::backend_wake_repark::replay(|i| {
            can_repark_expired(
                i.resuming,
                &i.observed_generation,
                &i.expected_generation,
                millis(i.observed_expiry),
                millis(i.expected_expiry),
                millis(i.now),
            )
        });
    }

    #[test]
    fn an_unresumed_wake_says_why_the_request_stays_parked() {
        use super::BackendWake;
        let rebound = BackendWake::OtherBackend {
            bound: "old".to_owned(),
        };
        assert_eq!(rebound.bound_backend(), Some("old"));
        assert!(rebound.unclaimed_reason().contains("bound to the backend"));
        for wake in [BackendWake::NotDue, rebound, BackendWake::Lost] {
            assert!(!wake.resumed());
            assert!(!wake.unclaimed_reason().contains("same-session"));
        }
    }

    #[test]
    fn resumed_turn_matches_the_generated_model() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().backend_resumed_turn_cases;
        assert!(cases.iter().any(|case| case.next > 1));
        for case in cases {
            assert_eq!(
                super::resumed_turn_index(case.completed.iter().copied()),
                case.next,
                "{case:?}"
            );
        }
    }

    #[test]
    fn healthy_wakeup_matches_the_generated_model() {
        let at = |millis: u64| {
            Utc.timestamp_millis_opt(i64::try_from(millis).unwrap())
                .single()
                .unwrap()
        };
        gents_lean_vocab::families::backend_wake_healthy_wake::replay(|i| {
            fresh_healthy(at(i.started_ms), at(i.now_ms), i.observed_ms.map(at))
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeResult {
    Resumed {
        generation: String,
        claim_cid: String,
        deadline: DateTime<Utc>,
        lease_ms: u64,
        background_cutoff: Option<u32>,
    },
    NotDue,
    /// Lean `RequestResumeAdmission.claimValid` admits a resumed deadline only
    /// on the backend of the original claim version, so a request parked
    /// before its agent was repointed is never rebound.
    OtherBackend {
        bound: String,
    },
    Lost,
}

/// A wake is bound to the parked backend and installs a new generation; the
/// caller supplies measured health for that backend or relies on the stored
/// fallback deadline.
pub async fn wake_parked_execution(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    backend_id: &str,
    healthy_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> anyhow::Result<WakeResult> {
    use gents_store::config_client::IdempotentTransactionRetry;
    let request_doc_id = request_doc_id.to_owned();
    let backend_id = backend_id.to_owned();
    let generation = uuid::Uuid::new_v4().to_string();
    access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "lifecycle.wake_backend_wait",
            move |txn| {
                let request_doc_id = request_doc_id.clone();
                let backend_id = backend_id.clone();
                let generation = generation.clone();
                Box::pin(async move {
                    wake_in_txn(
                        txn,
                        &request_doc_id,
                        &backend_id,
                        healthy_at,
                        Utc::now().max(now),
                        &generation,
                    )
                    .await
                })
            },
        )
        .await
}

async fn wake_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    backend_id: &str,
    healthy_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    generation: &str,
) -> anyhow::Result<WakeResult> {
    use anyhow::Context;
    use gents_protocol::request_lifecycle::RequestLifecycleState as State;
    use gents_protocol::row::AgentRequestRow;
    use gents_store::graphql::{escape_graphql_string, response_has_documents};

    let doc_id = escape_graphql_string(request_doc_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(
        filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
        _docID request_id node_did lifecycle_state backend_id deadline execution_generation
        execution_lease_secs execution_lease_expires_at interrupt_requested_at terminal_output
        background_completion_input_through_sequence
    }} }}"#
        ))
        .await?;
    let row = gents_store::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
        .context("backend wait request disappeared")?;
    if !matches!(
        row.lifecycle_state,
        Some(State::WaitingForBackend | State::ResumingBackend)
    ) {
        return Ok(WakeResult::Lost);
    }
    let metadata = load_wait_metadata(txn, request_doc_id)
        .await?
        .context("backend wait metadata is missing")?;
    anyhow::ensure!(
        row.node_did.as_deref() == Some(metadata.node_did.as_str()),
        "backend wait belongs to a different node"
    );
    if row.lifecycle_state == Some(State::ResumingBackend)
        && row.execution_generation.as_deref() == Some(generation)
    {
        let expiry = DateTime::parse_from_rfc3339(
            row.execution_lease_expires_at
                .as_deref()
                .context("resuming request is missing its lease")?,
        )?
        .with_timezone(&Utc);
        let deadline = DateTime::parse_from_rfc3339(
            row.deadline
                .as_deref()
                .context("resuming request is missing its deadline")?,
        )?
        .with_timezone(&Utc);
        if now >= expiry
            || now >= deadline
            || row.backend_id.as_deref() != Some(backend_id)
            || row
                .interrupt_requested_at
                .as_deref()
                .is_some_and(|v| !v.is_empty())
            || row.terminal_output.is_some()
        {
            return Ok(WakeResult::Lost);
        }
        let claim_cid = (!metadata.claim_cid.trim().is_empty())
            .then_some(metadata.claim_cid.as_str())
            .context("backend wait is missing its original claim version")?;
        return resumed_receipt(&row, generation, claim_cid, deadline);
    }
    if row.lifecycle_state != Some(State::WaitingForBackend)
        || row
            .interrupt_requested_at
            .as_deref()
            .is_some_and(|v| !v.is_empty())
        || row.terminal_output.is_some()
    {
        return Ok(WakeResult::Lost);
    }
    if row.backend_id.as_deref() != Some(backend_id) {
        return Ok(WakeResult::OtherBackend {
            bound: row.backend_id.clone().unwrap_or_default(),
        });
    }
    let claim_cid = Some(metadata.claim_cid.as_str())
        .filter(|cid| !cid.trim().is_empty())
        .context("backend wait is missing its original claim version")?;
    let deadline = DateTime::parse_from_rfc3339(
        row.deadline
            .as_deref()
            .context("missing request deadline")?,
    )?
    .with_timezone(&Utc);
    if now >= deadline {
        return Ok(WakeResult::Lost);
    }
    let retry_at = metadata.retry_at.as_str();
    let due = DateTime::parse_from_rfc3339(retry_at)?.with_timezone(&Utc) <= now;
    let wait_started = DateTime::parse_from_rfc3339(&metadata.wait_started_at)?.with_timezone(&Utc);
    let healthy_since_wait = fresh_healthy(wait_started, now, healthy_at);
    if !healthy_since_wait && !due {
        return Ok(WakeResult::NotDue);
    }
    let previous_generation = row
        .execution_generation
        .as_deref()
        .context("missing parked generation")?;
    anyhow::ensure!(
        previous_generation != generation,
        "backend wake generation must be fresh"
    );
    let lease_secs = row
        .execution_lease_secs
        .context("missing execution lease duration")?;
    anyhow::ensure!(lease_secs > 0, "invalid execution lease duration");
    let duration =
        Duration::try_seconds(lease_secs).context("execution lease duration overflows")?;
    let expiry = now
        .checked_add_signed(duration)
        .context("execution lease duration overflows")?
        .min(deadline);
    let interrupt_filter = match row.interrupt_requested_at.as_deref() {
        Some(value) => format!("\"{}\"", escape_graphql_string(value)),
        None => "null".to_string(),
    };
    let backend = escape_graphql_string(backend_id);
    let old_generation = escape_graphql_string(previous_generation);
    let generation_gql = escape_graphql_string(generation);
    let expiry = escape_graphql_string(&expiry.to_rfc3339());
    let result = txn
        .execute_local_response(&format!(
            r#"mutation {{ update_AgentRequest(docID: "{doc_id}",
        filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "waitingForBackend" }},
        backend_id: {{ _eq: "{backend}" }}, execution_generation: {{ _eq: "{old_generation}" }},
        interrupt_requested_at: {{ _eq: {interrupt_filter} }} }},
        input: {{ lifecycle_state: "resumingBackend", execution_generation: "{generation_gql}",
        execution_lease_expires_at: "{expiry}" }} ) {{ _docID }} }}"#
        ))
        .await?;
    Ok(
        if result
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            resumed_receipt(&row, generation, claim_cid, deadline)?
        } else {
            WakeResult::Lost
        },
    )
}

fn resumed_receipt(
    row: &gents_protocol::row::AgentRequestRow,
    generation: &str,
    claim_cid: &str,
    deadline: DateTime<Utc>,
) -> anyhow::Result<WakeResult> {
    use anyhow::Context;
    let lease_ms = row
        .execution_lease_secs
        .context("missing execution lease duration")?
        .checked_mul(1000)
        .filter(|ms| *ms > 0)
        .and_then(|ms| u64::try_from(ms).ok())
        .context("invalid execution lease duration")?;
    let background_cutoff = row
        .background_completion_input_through_sequence
        .map(u32::try_from)
        .transpose()
        .context("invalid background completion cutoff")?;
    Ok(WakeResult::Resumed {
        generation: generation.to_owned(),
        claim_cid: claim_cid.to_owned(),
        deadline,
        lease_ms,
        background_cutoff,
    })
}
