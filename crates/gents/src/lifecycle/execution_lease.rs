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
    /// `#[error(transparent)]` wrappers forward `source()` past the lease error
    /// and the stream processor's lifecycle source, so both are matched here.
    pub(crate) fn occurs_in(error: &(dyn std::error::Error + 'static)) -> bool {
        use gents_loop::stream_processor::StreamProcessorError;
        std::iter::successors(Some(error), |error| error.source()).any(|error| {
            error.is::<Self>()
                || matches!(
                    error.downcast_ref::<ExecutionLeaseError>(),
                    Some(ExecutionLeaseError::OwnershipLost(_))
                )
                || matches!(
                    error.downcast_ref::<StreamProcessorError>(),
                    Some(StreamProcessorError::Lifecycle(source)) if Self::occurs_in(&**source)
                )
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionLeaseError {
    #[error(transparent)]
    OwnershipLost(#[from] ExecutionOwnershipLost),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Config(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    DeadlineParse(#[from] chrono::ParseError),
    #[error("missing execution expiry")]
    MissingExpiry,
    #[error("missing execution state")]
    MissingState,
    #[error("missing execution generation")]
    MissingGeneration,
    #[error("missing request document")]
    MissingDocument,
    #[error("missing request state")]
    LifecycleMissing,
    #[error("missing request agent")]
    AgentMissing,
    #[error("missing request session")]
    SessionMissing,
    #[error("execution request disappeared")]
    ExecutionDisappeared,
    #[error("execution request purpose missing")]
    PurposeMissing,
    #[error("auxiliary execution cannot select a transcript message")]
    AuxiliarySelection,
    #[error("request deadline expired before natural completion")]
    DeadlineExpired,
    #[error("only revocation may select output by metadata")]
    MetadataSelectionForbidden,
    #[error("revocation terminal header sequence is ambiguous")]
    RevocationSequenceAmbiguous,
    #[error("terminal selection lacks exact owned assistant header")]
    ExactHeaderMissing,
    #[error("NoMessage cannot replace an owned assistant header during revocation")]
    NoMessageRevocation,
    #[error("NoMessage cannot replace a reconstructable owned assistant header")]
    NoMessageReplacement,
    #[error("auxiliary source has no unique closure")]
    AuxiliarySourceClosure,
    #[error("natural completion lost execution ownership")]
    NaturalCompletionLost,
}

impl crate::config_client::TransactionFailure for ExecutionLeaseError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Lifecycle(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            _ => None,
        }
    }
}

type LeaseResult<T, E = ExecutionLeaseError> = std::result::Result<T, E>;

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
        let node = self.node.clone();
        let doc_id = self.request.doc_id.clone();
        let generation = lease.generation.as_str().to_owned();
        // Relinquishment only expires the matching generation. Recovery remains
        // the terminal owner, including when a panic or task abort drops us.
        lease.runtime.spawn(async move {
            if let Err(error) =
                relinquish_execution_lease(&node, &doc_id, &generation, Utc::now()).await
            {
                tracing::warn!(%error, %doc_id, "could not promptly relinquish execution lease; durable expiry remains recoverable");
            }
        });
    }
}

async fn relinquish_execution_lease(
    node: &Arc<EmbeddedNode>,
    request_doc_id: &str,
    generation: &str,
    now: DateTime<Utc>,
) -> LeaseResult<()> {
    let doc_id = escape_graphql_string(request_doc_id);
    let generation = escape_graphql_string(generation);
    let expired = escape_graphql_string(&(now - chrono::Duration::milliseconds(1)).to_rfc3339());
    let now = escape_graphql_string(&now.to_rfc3339());
    let active = RequestLifecycleState::graphql_list([
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    ]);
    crate::config_client::ConfigAccess::write_local(
        node,
        "lifecycle.relinquish_execution_lease",
        &format!(r#"mutation {{ update_AgentRequest(
            docID: "{doc_id}", filter: {{ _docID: {{ _eq: "{doc_id}" }}, execution_generation: {{ _eq: "{generation}" }},
                lifecycle_state: {{ _in: {active} }}, execution_lease_expires_at: {{ _gt: "{now}" }} }},
            input: {{ execution_lease_expires_at: "{expired}" }}
        ) {{ _docID }} }}"#),
    )
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
pub(crate) struct RequestExecutionLease {
    pub(crate) generation: ExecutionGeneration,
    runtime: crate::drop_cleanup::DropCleanup,
}

impl RequestExecutionLease {
    pub(crate) fn new(generation: String) -> Self {
        Self {
            generation: ExecutionGeneration(generation),
            runtime: crate::drop_cleanup::DropCleanup::current(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestTerminalOutcome {
    Completed,
    Failed,
    Interrupted,
    Dead,
    Superseded,
}

impl RequestTerminalOutcome {
    #[cfg(test)]
    pub(crate) fn request_lifecycle_state(self) -> RequestLifecycleState {
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
    Revocation {
        generation: &'a str,
        expiry: &'a str,
    },
}

impl RequestExecutionLease {
    async fn decide_lease(
        &self,
        node: &EmbeddedNode,
        request_doc_id: &str,
        decide: fn(super::execution_policy::LeaseObservation<'_>, &str, i64) -> bool,
    ) -> LeaseResult<Option<bool>> {
        let Some(row) = super::query::request_view(node, request_doc_id).await? else {
            return Ok(None);
        };
        let expiry = row
            .execution_lease_expires_at
            .as_deref()
            .ok_or(ExecutionLeaseError::MissingExpiry)?;
        let observed = super::execution_policy::LeaseObservation {
            request: row
                .lifecycle_state
                .ok_or(ExecutionLeaseError::MissingState)?,
            generation: row
                .execution_generation
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingGeneration)?,
            deadline_ms: DateTime::parse_from_rfc3339(expiry)?.timestamp_millis(),
        };
        Ok(Some(decide(
            observed,
            self.generation.as_str(),
            Utc::now().timestamp_millis(),
        )))
    }

    pub(crate) async fn owns_execution(
        &self,
        node: &EmbeddedNode,
        request_doc_id: &str,
    ) -> LeaseResult<bool> {
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
    ) -> LeaseResult<Option<bool>> {
        self.execution_lease
            .as_ref()
            .ok_or(LifecycleError::NoActiveExecutionGeneration)?
            .decide_lease(&self.node, &self.request.doc_id, decide)
            .await
    }

    pub(crate) async fn validate_owned_execution(&self) -> LeaseResult<()> {
        if !self
            .decide_lease(super::execution_policy::authorize_producer_decision)
            .await?
            .ok_or(ExecutionLeaseError::ExecutionDisappeared)?
        {
            return Err(ExecutionOwnershipLost.into());
        }
        Ok(())
    }

    pub(crate) fn ownership_lost(&self) -> tokio_util::sync::CancellationToken {
        self.renewal_task
            .as_ref()
            .map(super::execution_renewal::RenewalTask::ownership_lost)
            .unwrap_or_default()
    }

    /// Whether this execution's generation still holds a live lease on its
    /// active request. A failure observed after the lease is lost belongs to
    /// the request's current owner, not to this execution.
    pub(crate) async fn owns_execution(&self) -> LeaseResult<bool> {
        self.execution_lease
            .as_ref()
            .ok_or(LifecycleError::NoActiveExecutionGeneration)?
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
    ) -> anyhow::Result<bool> {
        let generation = self.execution_generation()?.to_owned();
        let result = terminalize_execution_with_time(
            &self.node,
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
            Err(error) if error.is::<PendingQueueChanged>() => Ok(false),
            Err(error) => Err(error),
            Ok(TerminalizeResult::Lost) => Err(ExecutionLeaseError::NaturalCompletionLost.into()),
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
    ) -> anyhow::Result<TerminalizeResult> {
        let generation = self.execution_generation()?.to_string();
        let result = terminalize_execution(
            &self.node,
            &self.request.doc_id,
            TerminalAuthority::Owner(&generation),
            outcome,
            Some(selection),
            reason
                .or(self.failure_reason.as_deref())
                .unwrap_or_default(),
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

#[cfg(test)]
pub(crate) async fn revoke_execution_generation(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    selection: TerminalOutput,
    reason: &str,
) -> anyhow::Result<TerminalizeResult> {
    terminalize_execution(
        node,
        row.doc_id
            .as_deref()
            .ok_or(ExecutionLeaseError::MissingDocument)?,
        TerminalAuthority::Revocation {
            generation: row
                .execution_generation
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingGeneration)?,
            expiry: row
                .execution_lease_expires_at
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingExpiry)?,
        },
        outcome,
        Some(selection),
        reason,
    )
    .await
}

/// Revoke without discarding already-published output. Selection is made under
/// the same write gate as the generation CAS and uses immutable header metadata,
/// exactly as `terminalSelectionMetadataValid`; corrupt bytes remain untouched.
pub(crate) async fn revoke_execution_preserving_output(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> anyhow::Result<TerminalizeResult> {
    terminalize_execution(
        node,
        row.doc_id
            .as_deref()
            .ok_or(ExecutionLeaseError::MissingDocument)?,
        TerminalAuthority::Revocation {
            generation: row
                .execution_generation
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingGeneration)?,
            expiry: row
                .execution_lease_expires_at
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingExpiry)?,
        },
        outcome,
        None,
        reason,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn revoke_execution_preserving_output_at(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
    expected_generation: &str,
    fresh_generation: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<TerminalizeResult> {
    terminalize_execution_with_time(
        node,
        TerminalizeSpec {
            request_doc_id: row
                .doc_id
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingDocument)?,
            authority: TerminalAuthority::Revocation {
                generation: expected_generation,
                expiry: row
                    .execution_lease_expires_at
                    .as_deref()
                    .ok_or(ExecutionLeaseError::MissingExpiry)?,
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

async fn terminalize_execution(
    node: &std::sync::Arc<EmbeddedNode>,
    request_doc_id: &str,
    authority: TerminalAuthority<'_>,
    outcome: RequestTerminalOutcome,
    selection: Option<TerminalOutput>,
    reason: &str,
) -> anyhow::Result<TerminalizeResult> {
    terminalize_execution_with_time(
        node,
        TerminalizeSpec {
            request_doc_id,
            authority,
            outcome,
            selection,
            reason,
            fixture_now: None,
            fixture_fresh_generation: None,
            pending_fence: None,
        },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn terminalize_owned_at(
    node: &std::sync::Arc<EmbeddedNode>,
    request_doc_id: &str,
    generation: &str,
    outcome: RequestTerminalOutcome,
    selection: TerminalOutput,
    now: DateTime<Utc>,
) -> anyhow::Result<TerminalizeResult> {
    terminalize_execution_with_time(
        node,
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

async fn terminalize_execution_with_time(
    node: &std::sync::Arc<EmbeddedNode>,
    spec: TerminalizeSpec<'_>,
) -> anyhow::Result<TerminalizeResult> {
    let fresh_generation = spec
        .fixture_fresh_generation
        .map_or_else(ExecutionGeneration::fresh, |generation| {
            ExecutionGeneration(generation.to_owned())
        });
    let fresh_generation = &fresh_generation;
    let spec = &spec;
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::Standard,
        "lifecycle.terminalize_execution",
        move |txn| Box::pin(terminalize_execution_in_txn(txn, spec, fresh_generation)),
    )
    .await
}

pub(super) async fn stage_revoke_execution_preserving_output(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> anyhow::Result<TerminalizeResult> {
    terminalize_execution_in_txn(
        txn,
        &TerminalizeSpec {
            request_doc_id: row
                .doc_id
                .as_deref()
                .ok_or(ExecutionLeaseError::MissingDocument)?,
            authority: TerminalAuthority::Revocation {
                generation: row
                    .execution_generation
                    .as_deref()
                    .ok_or(ExecutionLeaseError::MissingGeneration)?,
                expiry: row
                    .execution_lease_expires_at
                    .as_deref()
                    .ok_or(ExecutionLeaseError::MissingExpiry)?,
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
}

async fn terminalize_execution_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    spec: &TerminalizeSpec<'_>,
    fresh_generation: &ExecutionGeneration,
) -> anyhow::Result<TerminalizeResult> {
    let request_doc_id = spec.request_doc_id;
    let authority = spec.authority;
    let outcome = spec.outcome;
    let selection = &spec.selection;
    let reason = spec.reason;
    let fixture_now = spec.fixture_now;
    let pending_fence = spec.pending_fence;
    use super::execution_policy::{
        authorize_execution_revocation, authorize_finalize, LeaseObservation,
    };
    use gents_protocol::output::{MessagePublication, MessageRole};

    if pending_fence.is_some() {
        crate::graphql::fence_arrivals(txn, "AgentRequest").await?;
    }
    let doc_id = escape_graphql_string(request_doc_id);
    let result = txn.execute_local_response(&format!(r#"{{ AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                _docID request_id purpose node_did requester_did session_id lifecycle_state
                execution_generation execution_lease_expires_at interrupt_requested_at terminal_output
            }} }}"#)).await?;
    let row = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")?
        .ok_or(ExecutionLeaseError::ExecutionDisappeared)?;
    let purpose = row.purpose.ok_or(ExecutionLeaseError::PurposeMissing)?;
    let auxiliary = purpose.is_auxiliary();
    if auxiliary
        && !selection
            .as_ref()
            .is_none_or(|selected| *selected == TerminalOutput::NoMessage)
    {
        return Err(ExecutionLeaseError::AuxiliarySelection.into());
    }
    let owner = row
        .execution_generation
        .as_deref()
        .ok_or(ExecutionLeaseError::MissingGeneration)?;
    let state = row
        .lifecycle_state
        .ok_or(ExecutionLeaseError::LifecycleMissing)?;
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
    let expiry = row
        .execution_lease_expires_at
        .as_deref()
        .ok_or(ExecutionLeaseError::MissingExpiry)?;
    let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
    let now = fixture_now.unwrap_or_else(Utc::now);
    let observed = LeaseObservation {
        request: state,
        generation: owner,
        deadline_ms: deadline,
    };
    let authorized = match authority {
        TerminalAuthority::Owner(expected) => authorize_finalize(
            observed,
            expected,
            now.timestamp_millis(),
            effective_outcome == RequestTerminalOutcome::Completed,
        ),
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
    if !authorized || row.terminal_output.is_some() {
        return Ok(TerminalizeResult::Lost);
    }
    let agent = row
        .node_did
        .as_deref()
        .ok_or(ExecutionLeaseError::AgentMissing)?;
    let session_id = row
        .session_id
        .as_deref()
        .ok_or(ExecutionLeaseError::SessionMissing)?;
    if let Some((expected, request_deadline)) = pending_fence {
        if !request_deadline.is_none_or(|deadline| Utc::now() < deadline) {
            return Err(ExecutionLeaseError::DeadlineExpired.into());
        }
        let actual = super::queue::pending_ids_in_txn(txn, agent, session_id).await?;
        if actual != expected {
            return Err(PendingQueueChanged.into());
        }
    }

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
    let eligible = |header: &&session::canonical_rows::TranscriptMessageRow| {
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
            if !matches!(authority, TerminalAuthority::Revocation { .. }) {
                return Err(ExecutionLeaseError::MetadataSelectionForbidden.into());
            }
            let latest = headers
                .iter()
                .filter(eligible)
                .max_by_key(|header| header.message.sequence);
            if let Some(latest) = latest {
                if headers
                    .iter()
                    .filter(eligible)
                    .filter(|header| header.message.sequence == latest.message.sequence)
                    .count()
                    != 1
                {
                    return Err(ExecutionLeaseError::RevocationSequenceAmbiguous.into());
                }
            }
            selected = latest.map_or(TerminalOutput::NoMessage, |header| {
                TerminalOutput::Message {
                    message_doc_id: header.doc_id.clone(),
                }
            });
            &selected
        }
    };
    if auxiliary && *selection != TerminalOutput::NoMessage {
        return Err(ExecutionLeaseError::AuxiliarySelection.into());
    }
    match selection {
        TerminalOutput::Message { message_doc_id } => {
            let matching = headers
                .iter()
                .filter(eligible)
                .filter(|header| &header.doc_id == message_doc_id)
                .collect::<Vec<_>>();
            if matching.len() != 1 {
                return Err(ExecutionLeaseError::ExactHeaderMissing.into());
            }
            if !matches!(authority, TerminalAuthority::Revocation { .. }) {
                reader.load_message(message_doc_id).await?;
            }
        }
        TerminalOutput::NoMessage => {
            for header in headers.iter().filter(eligible) {
                if matches!(authority, TerminalAuthority::Revocation { .. }) {
                    return Err(ExecutionLeaseError::NoMessageRevocation.into());
                }
                match reader.load_message(&header.doc_id).await {
                    Ok(_) => return Err(ExecutionLeaseError::NoMessageReplacement.into()),
                    Err(error)
                        if error
                            .downcast_ref::<gents_protocol::output::ReconstructionError>()
                            .is_some() => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    let terminal_generation = match authority {
        TerminalAuthority::Owner(_) => owner,
        TerminalAuthority::Revocation { .. } => fresh_generation.as_str(),
    };
    let generation = escape_graphql_string(owner);
    let next_generation = escape_graphql_string(terminal_generation);
    let expiry = escape_graphql_string(expiry);
    let timestamp = now.to_rfc3339();
    let timestamp_gql = escape_graphql_string(&timestamp);
    let outcome_reason = reason;
    let reason = escape_graphql_string(reason);
    let stopped_before_running = state.certifies_stopped_before_running(
        effective_outcome.request_state(),
        matches!(authority, TerminalAuthority::Owner(_)),
    );
    let target = escape_graphql_string(effective_outcome.request_state().as_str());
    let state = escape_graphql_string(state.as_str());
    let mutation = format!(
        r#"mutation($terminal_output: JSON) {{ update_AgentRequest(
                docID: "{doc_id}", filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
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
        return Ok(TerminalizeResult::Lost);
    }
    if !auxiliary {
        super::terminal_tools::account_tools_in_txn(
            txn,
            &mut reader,
            &row,
            &headers,
            owner,
            effective_outcome == RequestTerminalOutcome::Completed,
            &timestamp,
        )
        .await?;
        session::refresh_session_request_observation_in_txn(
            txn,
            agent,
            row.requester_did.as_deref(),
            session_id,
            request_doc_id,
            &row.request_id,
            &timestamp,
        )
        .await?;
    }
    crate::trigger_engine::durable::publish_request_outcome(
        txn,
        agent,
        &row.request_id,
        effective_outcome.request_state().as_str(),
        outcome_reason,
        &timestamp,
    )
    .await?;
    Ok(TerminalizeResult::Won)
}

pub(super) fn validate_auxiliary_sources_decided(
    request_doc_id: &str,
    records: &[crate::session::canonical_rows::OutputSegmentRow],
    purpose: gents_protocol::request_admission::RequestPurpose,
) -> anyhow::Result<()> {
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
        if closing.len() != 1 {
            return Err(ExecutionLeaseError::AuxiliarySourceClosure.into());
        }
        let closing = closing[0];
        let close = closing.segment.close.as_ref().expect("closing checked");
        match close {
            SourceClose::Closed { .. } => {
                crate::streaming::canonical::validate_closed_source_extent(
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

#[derive(Debug, thiserror::Error)]
#[error("pending input changed before natural completion")]
struct PendingQueueChanged;
