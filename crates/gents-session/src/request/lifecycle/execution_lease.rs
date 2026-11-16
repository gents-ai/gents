use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionGeneration(String);

#[derive(Debug, thiserror::Error)]
#[error("execution lease expired or ownership was revoked")]
pub struct ExecutionOwnershipLost;

impl ExecutionOwnershipLost {
    /// Whether the lost lease is the root or a context of an erased
    /// execution failure.
    pub fn occurs_in(error: &anyhow::Error) -> bool {
        error.is::<Self>()
    }
}

impl Drop for RequestLifecycle {
    fn drop(&mut self) {
        self.renewal_task.take();
        let Some(lease) = self.execution_lease.as_ref() else {
            return;
        };
        if matches!(
            self.state,
            LocalLifecycleState::Completed
                | LocalLifecycleState::Failed
                | LocalLifecycleState::Interrupted
        ) {
            return;
        }
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => {
                tracing::warn!(%error, "cannot relinquish request without its agent DID");
                return;
            }
        };
        let doc_id = self.request.doc_id.clone();
        let generation = lease.generation.as_str().to_owned();
        // Relinquishment only expires the matching generation. Recovery remains
        // the terminal owner, including when a panic or task abort drops us.
        lease.runtime.spawn(async move {
            if let Err(error) =
                relinquish_execution_lease(&access, &doc_id, &generation, Utc::now()).await
            {
                tracing::warn!(%error, %doc_id, "could not promptly relinquish execution lease; durable expiry remains recoverable");
            }
        });
    }
}

async fn relinquish_execution_lease(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    generation: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    use gents_protocol::document_query::{fields::agent_request as request, Filter, Input, Update};
    let expired = (now - chrono::Duration::milliseconds(1)).to_rfc3339();
    let now = now.to_rfc3339();
    let rendered = Update::doc(
        request_doc_id,
        Input::new().set(request::EXECUTION_LEASE_EXPIRES_AT, expired),
    )
    .filter(Filter::eq(request::DOC_ID, request_doc_id))
    .filter(Filter::eq(request::EXECUTION_GENERATION, generation))
    .filter(Filter::any_of(
        request::LIFECYCLE_STATE,
        [
            RequestLifecycleState::Claimed,
            RequestLifecycleState::Processing,
            RequestLifecycleState::ResumingBackend,
        ]
        .map(RequestLifecycleState::as_str),
    ))
    .filter(Filter::gt(request::EXECUTION_LEASE_EXPIRES_AT, now))
    .render();
    access
        .write_rendered("lifecycle.relinquish_execution_lease", &rendered)
        .await?;
    Ok(())
}

impl ExecutionGeneration {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct RequestExecutionLease {
    pub(crate) generation: ExecutionGeneration,
    actor: identity::Did,
    runtime: gents_store::drop_cleanup::DropCleanup,
}

impl RequestExecutionLease {
    pub fn new(generation: String, actor: identity::Did) -> Self {
        Self {
            generation: ExecutionGeneration(generation),
            actor,
            runtime: gents_store::drop_cleanup::DropCleanup::current(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestTerminalOutcome {
    Completed,
    Failed,
    Interrupted,
    Dead,
    Superseded,
}

impl RequestTerminalOutcome {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn request_lifecycle_state(self) -> RequestLifecycleState {
        self.request_state()
    }

    fn request_state(self) -> RequestLifecycleState {
        match self {
            Self::Completed => RequestLifecycleState::Completed,
            Self::Failed => RequestLifecycleState::Failed,
            Self::Interrupted => RequestLifecycleState::Interrupted,
            Self::Dead => RequestLifecycleState::Dead,
            Self::Superseded => RequestLifecycleState::Superseded,
        }
    }

    fn local_state(self) -> LocalLifecycleState {
        match self {
            Self::Completed => LocalLifecycleState::Completed,
            Self::Failed | Self::Dead | Self::Superseded => LocalLifecycleState::Failed,
            Self::Interrupted => LocalLifecycleState::Interrupted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalizeResult {
    Won,
    AlreadySame,
    Lost,
}

#[derive(Clone, Copy)]
enum TerminalAuthority<'a> {
    Owner(&'a str),
    Parked {
        generation: &'a str,
    },
    Revocation {
        generation: &'a str,
        expiry: &'a str,
    },
}

pub async fn terminalize_parked_request(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    generation: &str,
    interrupted: bool,
) -> Result<TerminalizeResult> {
    terminalize_execution_with_access(
        access,
        TerminalizeSpec {
            request_doc_id,
            authority: TerminalAuthority::Parked { generation },
            outcome: if interrupted {
                RequestTerminalOutcome::Interrupted
            } else {
                RequestTerminalOutcome::Failed
            },
            selection: None,
            reason: if interrupted {
                "request interrupted while waiting for provider"
            } else {
                "request deadline expired while waiting for provider"
            },
            fixture_now: None,
            fixture_fresh_generation: None,
            pending_fence: None,
        },
    )
    .await
}

impl RequestExecutionLease {
    async fn decide_lease(
        &self,
        node: &std::sync::Arc<EmbeddedNode>,
        request_doc_id: &str,
        decide: fn(super::execution_policy::LeaseObservation<'_>, &str, i64) -> bool,
    ) -> Result<Option<bool>> {
        let Some(row) = super::query::request_view(node, &self.actor, request_doc_id).await? else {
            return Ok(None);
        };
        if row.lifecycle_state == Some(RequestLifecycleState::WaitingForBackend) {
            return Ok(Some(false));
        }
        let expiry = row
            .execution_lease_expires_at
            .as_deref()
            .context("missing execution expiry")?;
        let observed = super::execution_policy::LeaseObservation {
            request: row.lifecycle_state.context("missing execution state")?,
            generation: row
                .execution_generation
                .as_deref()
                .context("missing execution generation")?,
            deadline_ms: DateTime::parse_from_rfc3339(expiry)?.timestamp_millis(),
        };
        Ok(Some(decide(
            observed,
            self.generation.as_str(),
            Utc::now().timestamp_millis(),
        )))
    }

    pub async fn owns_execution(
        &self,
        node: &std::sync::Arc<EmbeddedNode>,
        request_doc_id: &str,
    ) -> Result<bool> {
        Ok(self
            .decide_lease(node, request_doc_id, |observed, generation, now_ms| {
                super::execution_policy::renewable_lifecycle(observed.request)
                    && super::execution_policy::is_live(observed, generation, now_ms)
            })
            .await?
            .unwrap_or(false))
    }
}

impl RequestLifecycle {
    async fn decide_lease(
        &self,
        decide: fn(super::execution_policy::LeaseObservation<'_>, &str, i64) -> bool,
    ) -> Result<Option<bool>> {
        self.execution_lease
            .as_ref()
            .context("request has no active execution generation")?
            .decide_lease(&self.node, &self.request.doc_id, decide)
            .await
    }

    pub async fn validate_owned_execution(&self) -> Result<()> {
        if !self
            .decide_lease(super::execution_policy::authorize_producer_decision)
            .await?
            .context("execution request disappeared")?
        {
            return Err(ExecutionOwnershipLost.into());
        }
        Ok(())
    }

    pub fn ownership_lost(&self) -> tokio_util::sync::CancellationToken {
        self.renewal_task
            .as_ref()
            .map(super::execution_renewal::RenewalTask::ownership_lost)
            .unwrap_or_default()
    }

    /// Whether this execution's generation still holds a live lease on its
    /// active request. A failure observed after the lease is lost belongs to
    /// the request's current owner, not to this execution.
    pub async fn owns_execution(&self) -> Result<bool> {
        self.execution_lease
            .as_ref()
            .context("request has no active execution generation")?
            .owns_execution(&self.node, &self.request.doc_id)
            .await
    }

    /// Finishes an interactive request against its observed whole pending queue.
    /// DefraDB 980190c mutates the AgentRequest arrival head for local and P2P
    /// inserts in their document transaction. Reading that head inside the
    /// finalizer supplies the point-read conflict fence that RepeatableRead
    /// predicate scans alone cannot provide.
    pub(crate) async fn finish_natural_turn(
        &mut self,
        message_doc_id: &str,
        pending_request_doc_ids: &[String],
    ) -> Result<bool> {
        let generation = self.execution_generation()?.to_owned();
        let result = terminalize_execution(
            &self.access()?,
            TerminalizeSpec {
                request_doc_id: &self.request.doc_id,
                authority: TerminalAuthority::Owner(&generation),
                outcome: RequestTerminalOutcome::Completed,
                selection: Some(TerminalOutput::Message {
                    message_doc_id: message_doc_id.to_owned(),
                }),
                reason: "",
                fixture_now: None,
                fixture_fresh_generation: None,
                pending_fence: Some((pending_request_doc_ids, self.claimed_deadline_at)),
            },
        )
        .await;
        match result {
            Err(TerminalizeError::PendingQueueChanged) => Ok(false),
            Err(error) => Err(error.into_anyhow()),
            Ok(TerminalizeResult::Lost) => {
                anyhow::bail!("natural completion lost execution ownership")
            }
            Ok(_) => {
                self.state = LocalLifecycleState::Completed;
                Ok(true)
            }
        }
    }

    pub async fn terminalize_owned(
        &mut self,
        outcome: RequestTerminalOutcome,
        selection: TerminalOutput,
        reason: Option<&str>,
    ) -> Result<TerminalizeResult> {
        let generation = self.execution_generation()?.to_string();
        let result = terminalize_execution_with_access(
            &self.access()?,
            TerminalizeSpec {
                request_doc_id: &self.request.doc_id,
                authority: TerminalAuthority::Owner(&generation),
                outcome,
                selection: Some(selection),
                reason: reason
                    .or(self.failure_reason.as_deref())
                    .unwrap_or_default(),
                fixture_now: None,
                fixture_fresh_generation: None,
                pending_fence: None,
            },
        )
        .await?;
        if matches!(
            result,
            TerminalizeResult::Won | TerminalizeResult::AlreadySame
        ) {
            self.state = outcome.local_state();
        }
        Ok(result)
    }
}

/// Revoke without discarding already-published output. Selection is made under
/// the same write gate as the generation CAS and uses immutable header metadata,
/// exactly as `terminalSelectionMetadataValid`; corrupt bytes remain untouched.
#[cfg(test)]
pub(crate) async fn revoke_execution_generation(
    access: &gents_store::config_client::ConfigAccess,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    selection: TerminalOutput,
    reason: &str,
) -> Result<TerminalizeResult> {
    let actor = row
        .agent_did
        .as_deref()
        .context("request is missing agent DID")?;
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(actor),
        "revocation signer differs from request actor"
    );
    terminalize_execution_with_access(
        access,
        TerminalizeSpec {
            request_doc_id: row.doc_id.as_deref().context("missing request document")?,
            authority: TerminalAuthority::Revocation {
                generation: row
                    .execution_generation
                    .as_deref()
                    .context("missing execution generation")?,
                expiry: row
                    .execution_lease_expires_at
                    .as_deref()
                    .context("missing execution expiry")?,
            },
            outcome,
            selection: Some(selection),
            reason,
            fixture_now: None,
            fixture_fresh_generation: None,
            pending_fence: None,
        },
    )
    .await
}

pub async fn revoke_execution_preserving_output_with_access(
    access: &gents_store::config_client::ConfigAccess,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> Result<TerminalizeResult> {
    let actor = row
        .agent_did
        .as_deref()
        .context("request is missing agent DID")?;
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(actor),
        "recovery signer differs from request actor"
    );
    terminalize_execution_with_access(
        access,
        TerminalizeSpec {
            request_doc_id: row.doc_id.as_deref().context("missing request document")?,
            authority: TerminalAuthority::Revocation {
                generation: row
                    .execution_generation
                    .as_deref()
                    .context("missing execution generation")?,
                expiry: row
                    .execution_lease_expires_at
                    .as_deref()
                    .context("missing execution expiry")?,
            },
            outcome,
            selection: None,
            reason,
            fixture_now: None,
            fixture_fresh_generation: None,
            pending_fence: None,
        },
    )
    .await
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn revoke_execution_preserving_output_at_with_access(
    access: &gents_store::config_client::ConfigAccess,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
    expected_generation: &str,
    fresh_generation: &str,
    now: DateTime<Utc>,
) -> Result<TerminalizeResult> {
    terminalize_execution_with_access(
        access,
        TerminalizeSpec {
            request_doc_id: row.doc_id.as_deref().context("missing request document")?,
            authority: TerminalAuthority::Revocation {
                generation: expected_generation,
                expiry: row
                    .execution_lease_expires_at
                    .as_deref()
                    .context("missing execution expiry")?,
            },
            outcome,
            selection: None,
            reason,
            fixture_now: Some(now),
            fixture_fresh_generation: Some(fresh_generation),
            pending_fence: None,
        },
    )
    .await
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn terminalize_owned_at_with_access(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    generation: &str,
    outcome: RequestTerminalOutcome,
    selection: TerminalOutput,
    now: DateTime<Utc>,
) -> Result<TerminalizeResult> {
    terminalize_execution_with_access(
        access,
        TerminalizeSpec {
            request_doc_id,
            authority: TerminalAuthority::Owner(generation),
            outcome,
            selection: Some(selection),
            reason: "",
            fixture_now: Some(now),
            fixture_fresh_generation: None,
            pending_fence: None,
        },
    )
    .await
}

struct TerminalizeSpec<'a> {
    request_doc_id: &'a str,
    authority: TerminalAuthority<'a>,
    outcome: RequestTerminalOutcome,
    selection: Option<TerminalOutput>,
    reason: &'a str,
    fixture_now: Option<DateTime<Utc>>,
    fixture_fresh_generation: Option<&'a str>,
    pending_fence: Option<(&'a [String], Option<DateTime<Utc>>)>,
}

async fn terminalize_execution_with_access(
    access: &gents_store::config_client::ConfigAccess,
    spec: TerminalizeSpec<'_>,
) -> Result<TerminalizeResult> {
    terminalize_execution(access, spec)
        .await
        .map_err(TerminalizeError::into_anyhow)
}

async fn terminalize_execution(
    access: &gents_store::config_client::ConfigAccess,
    spec: TerminalizeSpec<'_>,
) -> std::result::Result<TerminalizeResult, TerminalizeError> {
    let fresh_generation = spec
        .fixture_fresh_generation
        .map_or_else(ExecutionGeneration::fresh, |generation| {
            ExecutionGeneration(generation.to_owned())
        });
    let fresh_generation = &fresh_generation;
    let spec = &spec;
    access
        .transact_idempotent(
            gents_store::config_client::IdempotentTransactionRetry::Standard,
            "lifecycle.terminalize_execution",
            move |txn| Box::pin(terminalize_execution_in_txn(txn, spec, fresh_generation)),
        )
        .await
}

pub async fn stage_revoke_execution_preserving_output(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> Result<TerminalizeResult> {
    terminalize_execution_in_txn(
        txn,
        &TerminalizeSpec {
            request_doc_id: row.doc_id.as_deref().context("missing request document")?,
            authority: TerminalAuthority::Revocation {
                generation: row
                    .execution_generation
                    .as_deref()
                    .context("missing execution generation")?,
                expiry: row
                    .execution_lease_expires_at
                    .as_deref()
                    .context("missing execution expiry")?,
            },
            outcome,
            selection: None,
            reason,
            fixture_now: None,
            fixture_fresh_generation: None,
            pending_fence: None,
        },
        &ExecutionGeneration::fresh(),
    )
    .await
    .map_err(TerminalizeError::into_anyhow)
}

/// A parked row has no lease to revoke; the parked terminal owner settles it
/// in the superseding fire's transaction.
pub async fn stage_supersede_parked_execution(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    row: &AgentRequestRow,
    reason: &str,
) -> Result<TerminalizeResult> {
    terminalize_execution_in_txn(
        txn,
        &TerminalizeSpec {
            request_doc_id: row.doc_id.as_deref().context("missing request document")?,
            authority: TerminalAuthority::Parked {
                generation: row
                    .execution_generation
                    .as_deref()
                    .context("missing parked generation")?,
            },
            outcome: RequestTerminalOutcome::Superseded,
            selection: None,
            reason,
            fixture_now: None,
            fixture_fresh_generation: None,
            pending_fence: None,
        },
        &ExecutionGeneration::fresh(),
    )
    .await
    .map_err(TerminalizeError::into_anyhow)
}

async fn terminalize_execution_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    spec: &TerminalizeSpec<'_>,
    fresh_generation: &ExecutionGeneration,
) -> std::result::Result<TerminalizeResult, TerminalizeError> {
    let request_doc_id = spec.request_doc_id;
    let authority = spec.authority;
    let outcome = spec.outcome;
    let selection = &spec.selection;
    let reason = spec.reason;
    let fixture_now = spec.fixture_now;
    let pending_fence = spec.pending_fence;
    use super::execution_policy::LeaseObservation;

    if pending_fence.is_some() {
        gents_store::graphql::fence_arrivals(txn, "AgentRequest").await?;
    }
    let doc_id = escape_graphql_string(request_doc_id);
    let result = {
        use gents_protocol::document_query::{
            fields::agent_request as request, Filter, Query, Selection,
        };
        let rendered = Query::select(
            Selection::doc_id()
                .field(request::REQUEST_ID)
                .field(request::PURPOSE)
                .field(request::NODE_DID)
                .field(request::REQUESTER_DID)
                .field(request::SESSION_ID)
                .field(request::LIFECYCLE_STATE)
                .field(request::EXECUTION_GENERATION)
                .field(request::EXECUTION_LEASE_EXPIRES_AT)
                .field(request::INTERRUPT_REQUESTED_AT)
                .field(request::TERMINAL_OUTPUT)
                .field(request::DEADLINE),
        )
        .filter(Filter::eq(request::DOC_ID, request_doc_id))
        .limit(1)
        .render();
        txn.run_local_response(&rendered).await?
    };
    let row = gents_store::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")
        .map_err(anyhow::Error::from)?
        .context("execution request disappeared")?;
    let purpose = row.purpose.context("execution request purpose missing")?;
    let auxiliary = purpose.is_auxiliary();
    if auxiliary
        && !selection
            .as_ref()
            .is_none_or(|selected| *selected == TerminalOutput::NoMessage)
    {
        return Err(
            anyhow::anyhow!("auxiliary execution cannot select a transcript message").into(),
        );
    }
    let owner = row
        .execution_generation
        .as_deref()
        .context("missing execution generation")?;
    let state = row.lifecycle_state.context("missing request state")?;
    let effective_outcome = if outcome == RequestTerminalOutcome::Failed
        && row
            .interrupt_requested_at
            .as_deref()
            .is_some_and(|v| !v.is_empty())
    {
        RequestTerminalOutcome::Interrupted
    } else {
        outcome
    };
    if state.is_terminal() {
        return Ok(
            if matches!(authority, TerminalAuthority::Owner(expected) if expected == owner)
                && state == effective_outcome.request_state()
                && row.terminal_output.as_ref() == selection.as_ref()
            {
                TerminalizeResult::AlreadySame
            } else {
                TerminalizeResult::Lost
            },
        );
    }
    let expiry = row.execution_lease_expires_at.as_deref();
    let lease_deadline = expiry
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(anyhow::Error::from)?
        .map(|time| time.timestamp_millis());
    if !matches!(authority, TerminalAuthority::Parked { .. }) && lease_deadline.is_none() {
        return Err(anyhow::anyhow!("missing execution expiry").into());
    }
    let now = fixture_now.unwrap_or_else(Utc::now);
    let observed = LeaseObservation {
        request: state,
        generation: owner,
        deadline_ms: lease_deadline.unwrap_or_default(),
    };
    let parked = ParkedObservation {
        leased: expiry.is_some(),
        interrupted: row
            .interrupt_requested_at
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        deadline: row
            .deadline
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(anyhow::Error::from)?
            .map(|deadline| deadline.with_timezone(&Utc)),
    };
    if !authorize_terminal_transition(
        observed,
        parked,
        authority,
        effective_outcome,
        now,
        fresh_generation,
        row.terminal_output.is_some(),
    )? {
        return Ok(TerminalizeResult::Lost);
    }
    let agent = row.node_did.as_deref().context("missing request agent")?;
    let session_id = row
        .session_id
        .as_deref()
        .context("missing request session")?;
    validate_pending_fence(txn, agent, session_id, pending_fence).await?;

    let headers = session::load_request_headers_in_txn(
        txn,
        session_id,
        agent,
        row.requester_did.as_deref(),
        request_doc_id,
    )
    .await?;
    if auxiliary && matches!(authority, TerminalAuthority::Owner(_)) {
        let records = super::recovery::request_segments(
            txn,
            request_doc_id,
            agent,
            session_id,
            row.requester_did.as_deref(),
        )
        .await?;
        validate_auxiliary_sources_decided(request_doc_id, &records, purpose)?;
    }
    let mut reader = session::TxnCanonicalReader::new(txn, agent, row.requester_did.as_deref());
    reader.observe_headers(&headers);
    let timestamp = now.to_rfc3339();
    let site = TerminalSite {
        request_doc_id,
        authority,
        outcome: effective_outcome,
        state,
        owner,
        expiry,
        interrupt_requested_at: row.interrupt_requested_at.as_deref(),
        deadline: row.deadline.as_deref(),
        reason: if matches!(authority, TerminalAuthority::Parked { .. })
            && effective_outcome == RequestTerminalOutcome::Interrupted
        {
            "request interrupted while waiting for provider"
        } else {
            reason
        },
        fresh_generation,
        auxiliary,
        agent,
        session_id,
        timestamp: &timestamp,
    };
    let (selection, reply_preview) =
        select_terminal_output(&mut reader, &headers, site, selection).await?;
    if !write_terminal_request(txn, site, &doc_id, &selection).await? {
        return Ok(TerminalizeResult::Lost);
    }
    write_terminal_dependents(txn, &mut reader, &row, &headers, site, &reply_preview).await?;
    Ok(TerminalizeResult::Won)
}

#[derive(Clone, Copy)]
struct TerminalSite<'a> {
    request_doc_id: &'a str,
    authority: TerminalAuthority<'a>,
    outcome: RequestTerminalOutcome,
    state: RequestLifecycleState,
    owner: &'a str,
    /// A parked row holds no lease.
    expiry: Option<&'a str>,
    interrupt_requested_at: Option<&'a str>,
    deadline: Option<&'a str>,
    reason: &'a str,
    fresh_generation: &'a ExecutionGeneration,
    auxiliary: bool,
    agent: &'a str,
    session_id: &'a str,
    timestamp: &'a str,
}

/// What a parked row's terminal owner decides on beside the lease observation.
#[derive(Clone, Copy)]
struct ParkedObservation {
    leased: bool,
    interrupted: bool,
    deadline: Option<DateTime<Utc>>,
}

fn authorize_terminal_transition(
    observed: super::execution_policy::LeaseObservation<'_>,
    parked: ParkedObservation,
    authority: TerminalAuthority<'_>,
    effective_outcome: RequestTerminalOutcome,
    now: DateTime<Utc>,
    fresh_generation: &ExecutionGeneration,
    has_terminal_output: bool,
) -> Result<bool> {
    use super::execution_policy::{
        authorize_execution_revocation, authorize_finalize, LeaseObservation,
    };
    let authorized = match authority {
        TerminalAuthority::Owner(expected) => authorize_finalize(
            observed,
            expected,
            now.timestamp_millis(),
            effective_outcome == RequestTerminalOutcome::Completed,
        ),
        TerminalAuthority::Parked { generation } => {
            super::execution_policy::can_finalize_parked(
                observed.request,
                effective_outcome.request_state(),
            ) && !parked.leased
                && observed.generation == generation
                && match effective_outcome {
                    RequestTerminalOutcome::Interrupted => parked.interrupted,
                    RequestTerminalOutcome::Failed => {
                        !parked.interrupted
                            && parked.deadline.is_some_and(|deadline| now >= deadline)
                    }
                    RequestTerminalOutcome::Superseded => true,
                    _ => false,
                }
        }
        TerminalAuthority::Revocation {
            generation,
            expiry: expected,
        } => authorize_execution_revocation(
            observed,
            LeaseObservation {
                generation,
                deadline_ms: DateTime::parse_from_rfc3339(expected)?.timestamp_millis(),
                ..observed
            },
            fresh_generation.as_str(),
            effective_outcome.request_state(),
        ),
    };
    Ok(authorized && !has_terminal_output)
}

async fn validate_pending_fence(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    agent: &str,
    session_id: &str,
    pending_fence: Option<(&[String], Option<DateTime<Utc>>)>,
) -> std::result::Result<(), TerminalizeError> {
    if let Some((expected, request_deadline)) = pending_fence {
        if !request_deadline.is_none_or(|deadline| Utc::now() < deadline) {
            return Err(
                anyhow::anyhow!("request deadline expired before natural completion").into(),
            );
        }
        let actual = super::queue::pending_ids_in_txn(txn, agent, session_id).await?;
        if actual != expected {
            return Err(TerminalizeError::PendingQueueChanged);
        }
    }
    Ok(())
}

async fn select_terminal_output(
    reader: &mut session::TxnCanonicalReader<'_, '_>,
    headers: &[gents_protocol::output::rows::TranscriptMessageRow],
    site: TerminalSite<'_>,
    selection: &Option<TerminalOutput>,
) -> Result<(TerminalOutput, Option<String>)> {
    use gents_protocol::output::{MessagePublication, MessageRole};
    let TerminalSite {
        authority,
        auxiliary,
        ..
    } = site;
    let eligible = |header: &&gents_protocol::output::rows::TranscriptMessageRow| {
        header.message.role == MessageRole::Assistant
            && matches!(
                header.message.publication,
                MessagePublication::RequestExecution { .. }
                    | MessagePublication::RequestRecovery { .. }
            )
    };
    let selected;
    let selection = match selection.as_ref() {
        Some(explicit) => explicit,
        None => {
            anyhow::ensure!(
                matches!(
                    authority,
                    TerminalAuthority::Revocation { .. } | TerminalAuthority::Parked { .. }
                ),
                "only revocation may select output by metadata"
            );
            let latest = headers
                .iter()
                .filter(eligible)
                .max_by_key(|header| header.message.sequence);
            if let Some(latest) = latest {
                anyhow::ensure!(
                    headers
                        .iter()
                        .filter(eligible)
                        .filter(|header| header.message.sequence == latest.message.sequence)
                        .count()
                        == 1,
                    "revocation terminal header sequence is ambiguous"
                );
            }
            selected = latest.map_or(TerminalOutput::NoMessage, |header| {
                TerminalOutput::Message {
                    message_doc_id: header.doc_id.clone(),
                }
            });
            &selected
        }
    };
    anyhow::ensure!(
        !auxiliary || *selection == TerminalOutput::NoMessage,
        "auxiliary execution cannot select a transcript message"
    );
    let mut reply_preview = None;
    match selection {
        TerminalOutput::Message { message_doc_id } => {
            let matching = headers
                .iter()
                .filter(eligible)
                .filter(|header| &header.doc_id == message_doc_id)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                matching.len() == 1,
                "terminal selection lacks exact owned assistant header"
            );
            if !matches!(
                authority,
                TerminalAuthority::Revocation { .. } | TerminalAuthority::Parked { .. }
            ) {
                let (_, message) = reader.load_message(message_doc_id).await?;
                reply_preview = session::visible_reply_preview(&message);
            }
        }
        TerminalOutput::NoMessage => {
            for header in headers.iter().filter(eligible) {
                anyhow::ensure!(
                    !matches!(
                        authority,
                        TerminalAuthority::Revocation { .. } | TerminalAuthority::Parked { .. }
                    ),
                    "NoMessage cannot replace an owned assistant header during revocation"
                );
                match reader.load_message(&header.doc_id).await {
                    Ok(_) => anyhow::bail!(
                        "NoMessage cannot replace a reconstructable owned assistant header"
                    ),
                    Err(error)
                        if gents_protocol::output::ReconstructionError::in_chain(
                            error.as_ref(),
                        )
                        .is_some() => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok((selection.clone(), reply_preview))
}

async fn write_terminal_request(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    site: TerminalSite<'_>,
    doc_id: &str,
    selection: &TerminalOutput,
) -> Result<bool> {
    let TerminalSite {
        authority,
        outcome: effective_outcome,
        state,
        owner,
        expiry,
        interrupt_requested_at,
        deadline,
        reason,
        fresh_generation,
        timestamp,
        ..
    } = site;
    let terminal_generation = match authority {
        TerminalAuthority::Owner(_) => owner,
        TerminalAuthority::Revocation { .. } | TerminalAuthority::Parked { .. } => {
            fresh_generation.as_str()
        }
    };
    let generation = escape_graphql_string(owner);
    let next_generation = escape_graphql_string(terminal_generation);
    let expiry_filter = expiry
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string());
    let parked_fence = if matches!(authority, TerminalAuthority::Parked { .. }) {
        let quoted = |value: Option<&str>| {
            value
                .map(|value| format!("\"{}\"", escape_graphql_string(value)))
                .unwrap_or_else(|| "null".to_string())
        };
        format!(
            ", interrupt_requested_at: {{ _eq: {} }}, deadline: {{ _eq: {} }}",
            quoted(interrupt_requested_at),
            quoted(deadline)
        )
    } else {
        String::new()
    };
    let timestamp_gql = escape_graphql_string(timestamp);
    let reason = escape_graphql_string(reason);
    let transition = state.transition_to(effective_outcome.request_state())?;
    let stopped_before_running = transition.from().certifies_stopped_before_running(
        transition.to(),
        matches!(authority, TerminalAuthority::Owner(_)),
    );
    let target = escape_graphql_string(transition.to().as_str());
    let state = escape_graphql_string(transition.from().as_str());
    let mutation = format!(
        r#"mutation($terminal_output: JSON) {{ update_AgentRequest(
                docID: "{doc_id}", filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: {expiry_filter} }}{parked_fence} }},
                input: {{ lifecycle_state: "{target}", execution_generation: "{next_generation}",
                    execution_lease_expires_at: "{timestamp_gql}", failure_reason: "{reason}",
                    terminalized_at: "{timestamp_gql}", terminal_redrive_attempts: 0,
                    terminal_output: $terminal_output, stopped_before_running: {stopped_before_running} }}
            ) {{ _docID }} }}"#
    );
    let result = txn
        .execute_with_variables(
            &mutation,
            &serde_json::json!({
                "terminal_output": selection
            }),
        )
        .await?;
    if !result
        .get("data")
        .and_then(|value| value.get("update_AgentRequest"))
        .is_some_and(response_has_documents)
    {
        return Ok(false);
    }
    Ok(true)
}

async fn write_terminal_dependents(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    reader: &mut session::TxnCanonicalReader<'_, '_>,
    row: &AgentRequestRow,
    headers: &[gents_protocol::output::rows::TranscriptMessageRow],
    site: TerminalSite<'_>,
    reply_preview: &Option<String>,
) -> Result<()> {
    let TerminalSite {
        request_doc_id,
        outcome: effective_outcome,
        owner,
        reason: outcome_reason,
        auxiliary,
        agent,
        session_id,
        timestamp,
        ..
    } = site;
    if !auxiliary {
        super::terminal_tools::account_tools_in_txn(
            txn,
            reader,
            row,
            headers,
            owner,
            effective_outcome == RequestTerminalOutcome::Completed,
            timestamp,
        )
        .await?;
        session::refresh_session_request_observation_in_txn(
            txn,
            agent,
            row.requester_did.as_deref(),
            session_id,
            session::SessionRequestRefresh {
                request_doc_id,
                request_id: &row.request_id,
                reply_preview: Some(reply_preview.as_deref()),
            },
            timestamp,
        )
        .await?;
    }
    crate::request::hooks::points::request_terminal(
        txn,
        agent,
        &row.request_id,
        effective_outcome.request_state(),
        outcome_reason,
        timestamp,
    )
    .await?;
    Ok(())
}

pub(super) fn validate_auxiliary_sources_decided(
    request_doc_id: &str,
    records: &[gents_protocol::output::rows::OutputSegmentRow],
    purpose: gents_protocol::request_admission::RequestPurpose,
) -> Result<()> {
    use gents_protocol::output::reconstruction::ObservedSegment;
    use gents_protocol::output::{OutputSource, SourceClose};
    let mut sources = Vec::new();
    for record in records {
        if record.segment.request_doc_id == request_doc_id
            && matches!(&record.segment.source, OutputSource::ProviderTurn { scope, .. }
                if scope.kind.request_purpose() == purpose)
            && !sources.contains(&record.segment.source)
        {
            sources.push(record.segment.source.clone());
        }
    }
    for source in sources {
        let scoped = records
            .iter()
            .filter(|record| {
                record.segment.request_doc_id == request_doc_id && record.segment.source == source
            })
            .collect::<Vec<_>>();
        let closing = scoped
            .iter()
            .filter(|record| record.segment.close.is_some())
            .collect::<Vec<_>>();
        if closing.len() > 1 {
            return Err(
                gents_protocol::output::ReconstructionError::ConflictingClosures {
                    request_doc_id: request_doc_id.to_owned(),
                    source: source.clone(),
                }
                .into(),
            );
        }
        anyhow::ensure!(closing.len() == 1, "auxiliary source has no unique closure");
        let closing = closing[0];
        let close = closing.segment.close.as_ref().expect("closing checked");
        match close {
            SourceClose::Closed { .. } => {
                crate::request::streaming::canonical::validate_closed_source_extent(
                    records,
                    request_doc_id,
                    &source,
                    closing,
                )?
            }
            SourceClose::Retracted => {
                let prefix = scoped
                    .iter()
                    .map(|record| ObservedSegment {
                        doc_id: &record.doc_id,
                        segment: &record.segment,
                    })
                    .collect::<Vec<_>>();
                gents_protocol::output::extent::inspect_open_source(
                    &prefix,
                    request_doc_id,
                    &source,
                    &closing.segment.writer,
                )?;
                if closing.segment.ordinal.is_some()
                    || !closing.segment.runs.is_empty()
                    || !closing.segment.payload.is_empty()
                {
                    return Err(
                        gents_protocol::output::ReconstructionError::InvalidStructure {
                            detail: "auxiliary retraction carries a data flush".to_owned(),
                        }
                        .into(),
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Why a terminal transition did not commit. An erased failure keeps its own
/// representation, so callers handed it as `anyhow` classify it as before.
#[derive(Debug, thiserror::Error)]
enum TerminalizeError {
    #[error("pending input changed before natural completion")]
    PendingQueueChanged,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl TerminalizeError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Other(error) => error,
            changed @ Self::PendingQueueChanged => anyhow::Error::new(changed),
        }
    }
}

impl From<gents_store::config_client::ConfigClientError> for TerminalizeError {
    fn from(error: gents_store::config_client::ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl gents_store::config_client::TransactionFailure for TerminalizeError {
    fn config_client_error(&self) -> Option<&gents_store::config_client::ConfigClientError> {
        match self {
            Self::Other(error) => {
                gents_store::config_client::TransactionFailure::config_client_error(error)
            }
            Self::PendingQueueChanged => None,
        }
    }

    fn rerun_requested(&self) -> Option<&gents_store::config_client::RerunNote> {
        match self {
            Self::Other(error) => {
                gents_store::config_client::TransactionFailure::rerun_requested(error)
            }
            Self::PendingQueueChanged => None,
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::RequestTerminalOutcome;

    #[test]
    fn request_execution_lease_outcome_request_state_matches_lean() {
        gents_lean_vocab::families::request_execution_lease_outcome_request_state::replay(|i| {
            RequestTerminalOutcome::request_state(i.outcome)
        });
    }
}
