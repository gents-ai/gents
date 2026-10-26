use super::*;
use gents_protocol::row::AgentRequestRow;

#[path = "canonical_recovery.rs"]
mod canonical_recovery;
pub(crate) use canonical_recovery::CanonicalRecoveryError;

pub(super) use canonical_recovery::request_segments;

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Config(#[from] crate::config_client::ConfigClientError),
    #[error("terminal AgentRequest is missing _docID")]
    MissingDocId,
    #[error("routed AgentRequest is missing requester_did")]
    MissingRequester,
    #[error("terminal AgentRequest is missing lifecycle_state")]
    MissingLifecycle,
    #[error("terminal AgentRequest is missing terminal_redrive_attempts")]
    MissingAttempts,
    #[error("terminal AgentRequest terminal_redrive_attempts must fit in u32")]
    AttemptsOutOfRange(#[source] std::num::TryFromIntError),
    #[error(transparent)]
    DurableTrigger(#[from] crate::trigger_engine::durable::DurableTriggerError),
    #[error(transparent)]
    Lifecycle(Box<crate::lifecycle::LifecycleError>),
}

impl From<crate::lifecycle::LifecycleError> for RecoveryError {
    fn from(error: crate::lifecycle::LifecycleError) -> Self {
        Self::Lifecycle(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for RecoveryError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Graphql(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            _ => None,
        }
    }
}

type Result<T, E = RecoveryError> = std::result::Result<T, E>;

#[cfg(test)]
pub(crate) use canonical_recovery::{
    recover_expired_generation_with_facts, RecoveryResult, RecoverySelectionChoice,
    RecoverySelectionRejected,
};

#[derive(Debug, Default)]
struct ActiveRequestRecoveryReport {
    requests: TerminalRepairReport,
    responses_recovered: usize,
}

impl RequestLifecycle {
    pub async fn recover_all(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<RecoveryReport> {
        let active = recover_active_requests(node, node_did).await?;
        crate::trigger_engine::durable::recover_outcomes(node, node_did).await?;
        let background_wakes_redriven = Self::redrive_failed_background_wakeups(node, node_did)
            .await?
            .redriven;
        Ok(RecoveryReport {
            responses_recovered: active.responses_recovered,
            requests_recovered: active.requests.repaired,
            background_wakes_redriven,
        })
    }

    /// Owner-scoped terminal-convergence re-drive (#664).
    ///
    /// Under `agent-target-host` replication a routed `AgentRequest` is replicated
    /// to its `requester_did` peer. Safety already holds (the watcher
    /// `node_did` filter never lets that peer claim a foreign replica), but
    /// liveness depends on the terminal head reaching that peer. DefraDB
    /// reports whether a replicator acknowledged a document's current heads,
    /// so a request that every replicator carrying it acknowledged is retired
    /// without re-assertion. Local-only requests, whose requester is
    /// absent or the owner itself, are excluded: no peer acts on their request
    /// state (#683).
    ///
    /// DefraDB authors no block for an unchanged field, so a re-assert is a
    /// write of the attempts counter alone. Its composite block has the
    /// terminal block as a DAG ancestor, so pushing it re-delivers the terminal
    /// state to a peer that missed it.
    ///
    /// BOUNDED. An unacknowledged peer may stay offline indefinitely, so each
    /// re-assert atomically advances the persisted `terminal_redrive_attempts`
    /// counter, and eligibility stops at [`TERMINAL_REDRIVE_CAP`] across process
    /// restarts. An acknowledged request is retired by writing the cap, so the
    /// candidate query never rescans it. Candidate ordering is
    /// `terminalized_at ASC`, not request creation time; exhausted rows leave the
    /// query, so bounded batches eventually cover an arbitrarily old request
    /// that terminalized late. A peer unavailable through the whole budget is
    /// repaired by a bounded full replicator replay when the pairing reconnects
    /// without acknowledging; that path authors no request delta.
    ///
    /// `node_did` MUST be the runtime's own DID: only the owner re-asserts its
    /// own documents; peers stay passive (a peer-authored delta to a foreign doc
    /// would fork the CRDT, not converge it). Only the attempts counter is
    /// written.
    pub async fn redrive_terminal_convergence(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<TerminalRedriveReport> {
        let escaped_node_did = escape_graphql_string(node_did);
        let terminal_states = RequestLifecycleState::terminal_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        node_did: {{ _eq: "{escaped_node_did}" }},
                        _and: [
                            {{ requester_did: {{ _neq: null }} }},
                            {{ requester_did: {{ _neq: "{escaped_node_did}" }} }}
                        ],
                        lifecycle_state: {{ _in: {terminal_states} }},
                        terminal_redrive_attempts: {{ _lt: {cap} }}
                    }},
                    order: [{{ terminalized_at: ASC }}, {{ request_id: ASC }}],
                    limit: {limit}
                ) {{
                    _docID
                    request_id
                    requester_did
                    lifecycle_state
                    terminal_redrive_attempts
                }}
            }}"#,
            limit = TERMINAL_REDRIVE_BATCH_LIMIT,
            cap = TERMINAL_REDRIVE_CAP,
        );

        let resp = crate::graphql::graphql_with_transaction_retry(
            node,
            &query,
            "querying terminal requests to re-drive",
        )
        .await?;

        let rows: Vec<AgentRequestRow> = crate::graphql::rows(&resp, "AgentRequest")?;

        let mut candidates: Vec<(String, String, String, RequestLifecycleState, u32)> = Vec::new();
        for row in rows {
            let doc_id = row.doc_id.ok_or(RecoveryError::MissingDocId)?;
            let requester_did = row.requester_did.ok_or(RecoveryError::MissingRequester)?;
            let lifecycle_state = row.lifecycle_state.ok_or(RecoveryError::MissingLifecycle)?;
            let attempts = row
                .terminal_redrive_attempts
                .ok_or(RecoveryError::MissingAttempts)
                .and_then(|attempts| {
                    u32::try_from(attempts).map_err(RecoveryError::AttemptsOutOfRange)
                })?;
            candidates.push((
                doc_id,
                row.request_id,
                requester_did,
                lifecycle_state,
                attempts,
            ));
        }

        let acknowledged = if candidates.is_empty() || node.p2p_arc().is_none() {
            std::collections::BTreeSet::new()
        } else {
            let documents = candidates
                .iter()
                .map(|(doc_id, _, requester_did, _, _)| (doc_id.clone(), requester_did.clone()))
                .collect::<Vec<_>>();
            crate::agent::p2p_reconcile::EmbeddedRemoteP2pAdmin::new(std::sync::Arc::clone(node))
                .requester_acknowledged("AgentRequest", &documents)
                .await
                .unwrap_or_else(|error| {
                    tracing::debug!(%error, "replication acknowledgement unavailable; re-driving every terminal candidate");
                    std::collections::BTreeSet::new()
                })
        };

        let mut report = TerminalRedriveReport {
            scanned: candidates.len(),
            ..Default::default()
        };
        for (doc_id, request_id, _, lifecycle_state, attempts) in &candidates {
            let is_acknowledged = acknowledged.contains(doc_id);
            let next_attempts = if is_acknowledged {
                TERMINAL_REDRIVE_CAP
            } else {
                attempts.saturating_add(1)
            };
            let escaped_doc_id = escape_graphql_string(doc_id);
            let escaped_lifecycle_state = escape_graphql_string(lifecycle_state.as_str());
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        docID: "{escaped_doc_id}", filter: {{
                            _docID: {{ _eq: "{escaped_doc_id}" }},
                            node_did: {{ _eq: "{escaped_node_did}" }},
                            _and: [
                                {{ requester_did: {{ _neq: null }} }},
                                {{ requester_did: {{ _neq: "{escaped_node_did}" }} }}
                            ],
                            lifecycle_state: {{ _eq: "{escaped_lifecycle_state}" }},
                            terminal_redrive_attempts: {{ _eq: {attempts} }}
                        }},
                        input: {{ terminal_redrive_attempts: {next_attempts} }}
                    ) {{ _docID }}
                }}"#,
            );

            let resp = crate::config_client::ConfigAccess::write_local(
                node,
                "lifecycle.redrive_terminal",
                &mutation,
            )
            .await;
            let resp = match resp {
                Ok(resp) => resp,
                Err(error) => {
                    tracing::warn!(doc_id = %doc_id, request_id = %request_id, lifecycle_state = %lifecycle_state, %error, "failed to re-drive terminal request convergence");
                    report.failed += 1;
                    continue;
                }
            };

            let updated = resp
                .pointer("/data/update_AgentRequest")
                .is_some_and(response_has_documents);
            if !updated {
                continue;
            }
            if is_acknowledged {
                report.acknowledged += 1;
                continue;
            }
            report.reasserted += 1;
            tracing::debug!(
                doc_id = %doc_id,
                request_id = %request_id,
                lifecycle_state = %lifecycle_state,
                terminal_redrive_attempts = next_attempts,
                "re-asserted terminal request state to converge replicas"
            );
        }

        Ok(report)
    }

    pub async fn repair_terminal_requests(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<TerminalRepairReport> {
        Ok(recover_active_requests(node, node_did).await?.requests)
    }
}

async fn recover_active_requests(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<ActiveRequestRecoveryReport> {
    let active_states = RequestLifecycleState::graphql_list([
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    ]);
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    lifecycle_state: {{ _in: {active_states} }}
                }}
            ) {{
                _docID
                request_id
                node_did
                requester_did
                agent_id
                session_id
                interrupt_requested_at
                execution_generation
                execution_lease_expires_at
            }}
        }}"#
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "querying active requests for recovery",
    )
    .await?;
    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    let now = chrono::Utc::now();
    let mut report = ActiveRequestRecoveryReport {
        requests: TerminalRepairReport {
            scanned: rows.len(),
            ..Default::default()
        },
        ..Default::default()
    };

    for row in &rows {
        let request_doc_id = match row.doc_id.as_deref() {
            Some(value) => value,
            None => {
                report.requests.failed += 1;
                tracing::warn!(
                    request_id = %row.request_id,
                    "active AgentRequest is missing _docID"
                );
                continue;
            }
        };
        let session_id = match row.session_id.as_deref() {
            Some(value) => value,
            None => {
                report.requests.failed += 1;
                tracing::warn!(
                    request_doc_id,
                    request_id = %row.request_id,
                    "active AgentRequest is missing session_id"
                );
                continue;
            }
        };

        let Some(expected_generation) = row
            .execution_generation
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            report.requests.failed += 1;
            tracing::warn!(
                request_doc_id,
                request_id = %row.request_id,
                "active AgentRequest is missing execution_generation"
            );
            continue;
        };
        let Some(expected_expiry) = row
            .execution_lease_expires_at
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            report.requests.failed += 1;
            tracing::warn!(
                request_doc_id,
                request_id = %row.request_id,
                execution_generation = expected_generation,
                "active AgentRequest is missing execution_lease_expires_at"
            );
            continue;
        };
        let expiry = match chrono::DateTime::parse_from_rfc3339(expected_expiry) {
            Ok(value) => value.with_timezone(&chrono::Utc),
            Err(error) => {
                report.requests.failed += 1;
                tracing::warn!(
                    request_doc_id,
                    request_id = %row.request_id,
                    execution_generation = expected_generation,
                    execution_lease_expires_at = expected_expiry,
                    error = %error,
                    "active AgentRequest has malformed execution_lease_expires_at"
                );
                continue;
            }
        };
        if expiry > now {
            report.requests.awaiting_outcome += 1;
            continue;
        }

        match canonical_recovery::recover_expired_generation(
            node,
            row,
            expected_generation,
            expected_expiry,
        )
        .await
        {
            Ok(canonical_recovery::RecoveryResult::Won { published }) => {
                report.requests.repaired += 1;
                report.responses_recovered += published;
            }
            Ok(canonical_recovery::RecoveryResult::Lost) => {
                report.requests.awaiting_outcome += 1;
            }
            Err(error) if definitive_output_corruption(&error) => {
                // Failed recovery rolled back. Revoke only the same observed
                // lease tuple; a renewal/recovery winner makes this lose. This
                // metadata-only escape never repairs or chooses payload twins.
                match super::execution_lease::revoke_execution_preserving_output(
                    node,
                    row,
                    super::RequestTerminalOutcome::Dead,
                    &format!("canonical output integrity failure: {error}"),
                )
                .await
                {
                    Ok(super::TerminalizeResult::Won) => report.requests.repaired += 1,
                    Ok(_) => report.requests.awaiting_outcome += 1,
                    Err(revoke_error) => {
                        report.requests.failed += 1;
                        tracing::warn!(request_doc_id, %error, %revoke_error,
                            "could not revoke corrupted expired execution");
                    }
                }
            }
            Err(error) => {
                report.requests.failed += 1;
                tracing::warn!(
                    request_doc_id,
                    request_id = %row.request_id,
                    session_id,
                    execution_generation = expected_generation,
                    %error,
                    "failed canonical expired-generation recovery"
                );
            }
        }
    }

    Ok(report)
}

/// Missing replica dependencies and access/storage failures are not proof of
/// corruption. Only typed contradictory or malformed immutable facts authorize
/// the model's metadata-only revocation escape.
fn definitive_output_corruption(error: &(dyn std::error::Error + 'static)) -> bool {
    use gents_protocol::error_chain::find_source;
    use gents_protocol::output::recovery::RecoveryPlanError;
    use gents_protocol::output::ReconstructionError;
    matches!(
        find_source::<RecoveryPlanError>(error),
        Some(
            RecoveryPlanError::IdentityConflict { .. }
                | RecoveryPlanError::ConflictingOrdinal { .. }
                | RecoveryPlanError::InvalidWriter
                | RecoveryPlanError::MalformedExtent
        )
    ) || matches!(
        crate::session::find_reconstruction_error(error),
        Some(
            ReconstructionError::ConflictingSegments { .. }
                | ReconstructionError::ConflictingClosures { .. }
                | ReconstructionError::InvalidWriter { .. }
                | ReconstructionError::ExtentMismatch { .. }
                | ReconstructionError::InvalidPayload { .. }
                | ReconstructionError::InvalidStructure { .. }
        )
    )
}
