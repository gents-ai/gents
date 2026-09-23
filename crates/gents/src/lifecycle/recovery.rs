use super::*;
use anyhow::Context;
use gents_protocol::row::AgentRequestRow;

#[path = "canonical_recovery.rs"]
mod canonical_recovery;

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
    pub async fn recover_all(node: &EmbeddedNode, agent_did: &str) -> Result<RecoveryReport> {
        let active = recover_active_requests(node, agent_did).await?;
        let background_wakes_redriven = Self::redrive_failed_background_wakeups(node, agent_did)
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
    /// Under `subagent-host` replication a routed `AgentRequest` is replicated
    /// to its `requester_did` peer. Safety already holds (the watcher
    /// `agent_did` filter never lets that peer claim a foreign replica), but
    /// liveness does not: when the owner terminalizes, the terminal delta
    /// reaches the requester via a single one-shot PushLog that can drop, and
    /// there is no per-doc anti-entropy on a running peer (defradb.rs#1074) to
    /// re-request it. This re-drive is the owner side of the fix — periodically
    /// re-asserting the current terminal value of recently-terminalized routed
    /// requests. Local-only requests are excluded because no peer consumes
    /// their request state (#683). A same-value re-write is a genuine
    /// higher-priority CRDT delta (it does not no-op), so it flows through the
    /// normal PushLog path and a lagging requester accepts it (LWW, higher
    /// priority ⇒ applied).
    ///
    /// BOUNDED, NOT CONVERGENCE-OBSERVING. The owner has no back-channel telling
    /// it whether a peer caught up. Each successful re-assert atomically advances
    /// the persisted `terminal_redrive_attempts` counter, and eligibility stops
    /// at [`TERMINAL_REDRIVE_CAP`] across process restarts. Candidate ordering is
    /// `terminalized_at ASC`, not request creation time; exhausted rows leave the
    /// query, so bounded batches eventually cover an arbitrarily old request that
    /// terminalized late. A peer unavailable through the whole budget is repaired
    /// by a bounded full replicator replay when the pairing reconnects; that path
    /// authors no same-value request delta and therefore grows no request history.
    ///
    /// `agent_did` MUST be the runtime's own DID: only the owner re-asserts its
    /// own documents; peers stay passive (a peer-authored delta to a foreign doc
    /// would fork the CRDT, not converge it). `agent_did` itself is never
    /// written (it is `@immutable`); only the mutable terminal `lifecycle_state`
    /// and bounded attempt counter are written together in one document update.
    ///
    pub async fn redrive_terminal_convergence(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<TerminalRedriveReport> {
        let escaped_agent_did = escape_graphql_string(agent_did);
        let terminal_states = RequestLifecycleState::terminal_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        requester_did: {{ _neq: null }},
                        lifecycle_state: {{ _in: {terminal_states} }},
                        terminal_redrive_attempts: {{ _lt: {cap} }}
                    }},
                    order: [{{ terminalized_at: ASC }}, {{ request_id: ASC }}],
                    limit: {limit}
                ) {{
                    _docID
                    request_id
                    lifecycle_state
                    terminal_redrive_attempts
                }}
            }}"#,
            limit = TERMINAL_REDRIVE_BATCH_LIMIT,
            cap = TERMINAL_REDRIVE_CAP,
        );

        let resp = node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("querying terminal requests to re-drive: {:?}", resp.errors);
        }

        let rows: Vec<AgentRequestRow> = crate::graphql::rows(&resp, "AgentRequest")?;

        let mut candidates: Vec<(String, String, RequestLifecycleState, u32)> = Vec::new();
        for row in rows {
            let doc_id = row
                .doc_id
                .context("terminal AgentRequest is missing _docID")?;
            let lifecycle_state = row
                .lifecycle_state
                .context("terminal AgentRequest is missing lifecycle_state")?;
            let attempts = row
                .terminal_redrive_attempts
                .context("terminal AgentRequest is missing terminal_redrive_attempts")
                .and_then(|attempts| {
                    u32::try_from(attempts)
                        .context("terminal AgentRequest terminal_redrive_attempts must fit in u32")
                })?;
            candidates.push((doc_id, row.request_id, lifecycle_state, attempts));
        }

        let scanned = candidates.len();
        let mut reasserted = 0usize;
        let mut failed = 0usize;
        for (doc_id, request_id, lifecycle_state, attempts) in &candidates {
            let next_attempts = attempts.saturating_add(1);
            let escaped_doc_id = escape_graphql_string(doc_id);
            let escaped_lifecycle_state = escape_graphql_string(lifecycle_state.as_str());
            // Defense-in-depth: the candidate query is already `agent_did == self`
            // scoped, but keep the mutation itself owner-scoped too, matching the
            // queue.rs seam guards — a re-drive must never touch a foreign replica.
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            _docID: {{ _eq: "{escaped_doc_id}" }},
                            agent_did: {{ _eq: "{escaped_agent_did}" }},
                            requester_did: {{ _neq: null }},
                            lifecycle_state: {{ _eq: "{escaped_lifecycle_state}" }},
                            terminal_redrive_attempts: {{ _eq: {attempts} }}
                        }},
                        input: {{
                            lifecycle_state: "{escaped_lifecycle_state}",
                            terminal_redrive_attempts: {next_attempts}
                        }}
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
                    failed += 1;
                    continue;
                }
            };

            let updated = resp
                .pointer("/data/update_AgentRequest")
                .is_some_and(response_has_documents);
            if !updated {
                continue;
            }
            reasserted += 1;
            tracing::debug!(
                doc_id = %doc_id,
                request_id = %request_id,
                lifecycle_state = %lifecycle_state,
                terminal_redrive_attempts = next_attempts,
                "re-asserted terminal request state to converge replicas"
            );
        }

        Ok(TerminalRedriveReport {
            reasserted,
            scanned,
            failed,
        })
    }

    pub async fn repair_terminal_requests(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<TerminalRepairReport> {
        Ok(recover_active_requests(node, agent_did).await?.requests)
    }
}

async fn recover_active_requests(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ActiveRequestRecoveryReport> {
    let active_states = RequestLifecycleState::graphql_list([
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    ]);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    lifecycle_state: {{ _in: {active_states} }}
                }}
            ) {{
                _docID
                request_id
                agent_did
                requester_did
                behavior_id
                session_id
                interrupt_requested_at
                execution_generation
                execution_lease_expires_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "querying active requests for recovery: {:?}",
            response.errors
        );
    }
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
fn definitive_output_corruption(error: &anyhow::Error) -> bool {
    use gents_protocol::output::recovery::RecoveryPlanError;
    use gents_protocol::output::ReconstructionError;
    matches!(
        error.downcast_ref::<RecoveryPlanError>(),
        Some(
            RecoveryPlanError::IdentityConflict { .. }
                | RecoveryPlanError::ConflictingOrdinal { .. }
                | RecoveryPlanError::InvalidWriter
                | RecoveryPlanError::MalformedExtent
        )
    ) || matches!(
        error.downcast_ref::<ReconstructionError>(),
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
