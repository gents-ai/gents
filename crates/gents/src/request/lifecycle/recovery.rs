use super::*;
use anyhow::Context;
use gents_protocol::row::AgentRequestRow;

#[path = "canonical_recovery.rs"]
mod canonical_recovery;

pub(super) use canonical_recovery::request_segments;

#[cfg(test)]
pub(crate) use canonical_recovery::{
    recover_expired_generation_with_access_for_test, RecoveryResult, RecoverySelectionChoice,
    RecoverySelectionRejected,
};

#[derive(Debug, Default)]
struct ActiveRequestRecoveryReport {
    requests: TerminalRepairReport,
    responses_recovered: usize,
}

impl RequestLifecycle {
    pub async fn recover_all_with_identities(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        identities: &[std::sync::Arc<dyn crate::store::identity::NodeIdentity>],
    ) -> Result<RecoveryReport> {
        let active = recover_active_requests(node, node_did, identities).await?;
        let background_wakes_redriven =
            Self::redrive_failed_background_wakeups_with_identities(node, node_did, identities)
                .await?
                .redriven;
        Ok(RecoveryReport {
            responses_recovered: active.responses_recovered,
            requests_recovered: active.requests.repaired,
            background_wakes_redriven,
        })
    }

    pub async fn repair_terminal_requests_with_identities(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        identities: &[std::sync::Arc<dyn crate::store::identity::NodeIdentity>],
    ) -> Result<TerminalRepairReport> {
        Ok(recover_active_requests(node, node_did, identities)
            .await?
            .requests)
    }
}

async fn recover_active_requests(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[std::sync::Arc<dyn crate::store::identity::NodeIdentity>],
) -> Result<ActiveRequestRecoveryReport> {
    let accesses = configured_agent_accesses(node, node_did, identities)?;
    let active_states = RequestLifecycleState::graphql_list([
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    ]);
    let escaped_node_did = escape_graphql_string(node_did);
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (actor, access) in &accesses {
        let escaped_agent_did = escape_graphql_string(actor);
        let query = format!(
            r#"{{
            AgentRequest(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    lifecycle_state: {{ _in: {active_states} }}
                }}
            ) {{
                _docID
                request_id
                node_did
                requester_did
                agent_did
                session_id
                interrupt_requested_at
                execution_generation
                execution_lease_expires_at
            }}
        }}"#
        );
        let response = access.execute(&query).await?;
        let agent_rows: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())
                .context("decoding active requests for recovery")?;
        for row in agent_rows {
            let doc_id = row
                .doc_id
                .as_ref()
                .context("active AgentRequest is missing _docID")?;
            if seen.insert(doc_id.clone()) {
                rows.push(row);
            }
        }
    }
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

        let agent_did = row
            .agent_did
            .as_deref()
            .context("request is missing agent DID")?;
        let access = accesses
            .get(agent_did)
            .context("request agent signer unavailable")?;
        match canonical_recovery::recover_expired_generation(
            access,
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
                match super::execution_lease::revoke_execution_preserving_output_with_access(
                    access,
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

/// Resolve only identities provided by the validated runtime snapshot.
pub(super) fn configured_agent_accesses(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[std::sync::Arc<dyn crate::store::identity::NodeIdentity>],
) -> Result<std::collections::BTreeMap<String, crate::ConfigAccess>> {
    anyhow::ensure!(
        node.node_identity_did() == Some(node_did),
        "recovery node identity mismatch"
    );
    let mut accesses = std::collections::BTreeMap::new();
    for identity in identities {
        let did = identity.did();
        anyhow::ensure!(
            did != node_did,
            "runtime identity cannot recover agent work"
        );
        let actor = identity::Did::new(did)?;
        let access = match crate::ConfigAccess::local_managed_agent(
            node.clone(),
            actor,
            identity.as_ref(),
        ) {
            Ok(access) => access,
            Err(error) => {
                tracing::warn!(agent_did = %did, %error, "agent recovery signing capability unavailable");
                continue;
            }
        };
        anyhow::ensure!(
            accesses.insert(did.to_owned(), access).is_none(),
            "duplicate recovery agent identity"
        );
    }
    Ok(accesses)
}
