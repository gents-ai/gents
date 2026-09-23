use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionGeneration(String);

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
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let node = self.node.clone();
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let generation = escape_graphql_string(lease.generation.as_str());
        // Relinquishment only expires the matching generation. Recovery remains
        // the terminal owner, including when a panic or task abort drops us.
        runtime.spawn(async move {
            let expired = (Utc::now() - chrono::Duration::milliseconds(1)).to_rfc3339();
            let active = RequestLifecycleState::graphql_list([RequestLifecycleState::Claimed, RequestLifecycleState::Processing]);
            let mutation = format!(r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, execution_generation: {{ _eq: "{generation}" }},
                    lifecycle_state: {{ _in: {active} }} }},
                input: {{ execution_lease_expires_at: "{expired}" }}
            ) {{ _docID }} }}"#);
            if let Err(error) = crate::config_client::ConfigAccess::write_local(
                &node,
                "lifecycle.relinquish_execution_lease",
                &mutation,
            )
            .await
            {
                tracing::warn!(%error, %doc_id, "could not promptly relinquish execution lease; durable expiry remains recoverable");
            }
        });
    }
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
}

impl RequestExecutionLease {
    pub(crate) fn new(generation: String) -> Self {
        Self {
            generation: ExecutionGeneration(generation),
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

impl RequestLifecycle {
    pub(crate) async fn validate_owned_execution(&self) -> Result<()> {
        let row = self
            .request_view()
            .await?
            .context("execution request disappeared")?;
        let expiry = row
            .execution_lease_expires_at
            .as_deref()
            .context("missing execution expiry")?;
        let deadline = DateTime::parse_from_rfc3339(expiry)?;
        anyhow::ensure!(
            super::execution_policy::authorize_producer_decision(
                super::execution_policy::LeaseObservation {
                    request: row.lifecycle_state.context("missing execution state")?,
                    generation: row
                        .execution_generation
                        .as_deref()
                        .context("missing execution generation")?,
                    deadline_ms: deadline.timestamp_millis(),
                },
                self.execution_generation()?,
                Utc::now().timestamp_millis(),
            ),
            "execution lease expired or ownership was revoked"
        );
        Ok(())
    }

    pub async fn terminalize_owned(
        &mut self,
        outcome: RequestTerminalOutcome,
        selection: TerminalOutput,
        reason: Option<&str>,
    ) -> Result<TerminalizeResult> {
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

/// LatestOnly and child deadlines revoke the observed execution through the
/// same atomic terminal owner. Unlike recovery, revocation may cancel a live lease.
pub(crate) async fn revoke_execution_generation(
    node: &EmbeddedNode,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    selection: TerminalOutput,
    reason: &str,
) -> Result<TerminalizeResult> {
    terminalize_execution(
        node,
        row.doc_id.as_deref().context("missing request document")?,
        TerminalAuthority::Revocation {
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
        Some(selection),
        reason,
    )
    .await
}

/// Revoke without discarding already-published output. Selection is made under
/// the same write gate as the generation CAS and uses immutable header metadata,
/// exactly as `terminalSelectionMetadataValid`; corrupt bytes remain untouched.
pub(crate) async fn revoke_execution_preserving_output(
    node: &EmbeddedNode,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> Result<TerminalizeResult> {
    terminalize_execution(
        node,
        row.doc_id.as_deref().context("missing request document")?,
        TerminalAuthority::Revocation {
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
        None,
        reason,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn revoke_execution_preserving_output_at(
    node: &EmbeddedNode,
    row: &AgentRequestRow,
    outcome: RequestTerminalOutcome,
    reason: &str,
    expected_generation: &str,
    fresh_generation: &str,
    now: DateTime<Utc>,
) -> Result<TerminalizeResult> {
    terminalize_execution_with_time(
        node,
        row.doc_id.as_deref().context("missing request document")?,
        TerminalAuthority::Revocation {
            generation: expected_generation,
            expiry: row
                .execution_lease_expires_at
                .as_deref()
                .context("missing execution expiry")?,
        },
        outcome,
        None,
        reason,
        Some(now),
        Some(fresh_generation),
    )
    .await
}

async fn terminalize_execution(
    node: &EmbeddedNode,
    request_doc_id: &str,
    authority: TerminalAuthority<'_>,
    outcome: RequestTerminalOutcome,
    selection: Option<TerminalOutput>,
    reason: &str,
) -> Result<TerminalizeResult> {
    terminalize_execution_with_time(
        node,
        request_doc_id,
        authority,
        outcome,
        selection,
        reason,
        None,
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn terminalize_owned_at(
    node: &EmbeddedNode,
    request_doc_id: &str,
    generation: &str,
    outcome: RequestTerminalOutcome,
    selection: TerminalOutput,
    now: DateTime<Utc>,
) -> Result<TerminalizeResult> {
    terminalize_execution_with_time(
        node,
        request_doc_id,
        TerminalAuthority::Owner(generation),
        outcome,
        Some(selection),
        "",
        Some(now),
        None,
    )
    .await
}

async fn terminalize_execution_with_time(
    node: &EmbeddedNode,
    request_doc_id: &str,
    authority: TerminalAuthority<'_>,
    outcome: RequestTerminalOutcome,
    selection: Option<TerminalOutput>,
    reason: &str,
    fixture_now: Option<DateTime<Utc>>,
    fixture_fresh_generation: Option<&str>,
) -> Result<TerminalizeResult> {
    use super::execution_policy::{
        authorize_execution_revocation, authorize_finalize, LeaseObservation,
    };
    use gents_protocol::output::{MessagePublication, MessageRole};
    let fresh_generation = fixture_fresh_generation
        .map_or_else(ExecutionGeneration::fresh, |generation| {
            ExecutionGeneration(generation.to_owned())
        });
    let fresh_generation = &fresh_generation;
    let selection = &selection;
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node, None, crate::config_client::IdempotentTransactionRetry::Standard,
        "lifecycle.terminalize_execution",
        move |txn| Box::pin(async move {
            let doc_id = escape_graphql_string(request_doc_id);
            let result = txn.execute_local_response(&format!(r#"{{ AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                _docID request_id agent_did requester_did session_id lifecycle_state
                execution_generation execution_lease_expires_at interrupt_requested_at terminal_output
            }} }}"#)).await?;
            let row = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")?
                .context("execution request disappeared")?;
            let owner = row.execution_generation.as_deref().context("missing execution generation")?;
            let state = row.lifecycle_state.context("missing request state")?;
            let effective_outcome = if outcome == RequestTerminalOutcome::Failed
                && row.interrupt_requested_at.as_deref().is_some_and(|v| !v.is_empty()) {
                RequestTerminalOutcome::Interrupted
            } else { outcome };
            if state.is_terminal() {
                return Ok(if matches!(authority, TerminalAuthority::Owner(expected) if expected == owner)
                    && state == effective_outcome.request_state() && row.terminal_output.as_ref() == selection.as_ref() {
                    TerminalizeResult::AlreadySame
                } else { TerminalizeResult::Lost });
            }
            let expiry = row.execution_lease_expires_at.as_deref().context("missing execution expiry")?;
            let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
            let now = fixture_now.unwrap_or_else(Utc::now);
            let observed = LeaseObservation { request: state, generation: owner, deadline_ms: deadline };
            let authorized = match authority {
                TerminalAuthority::Owner(expected) => authorize_finalize(
                    observed, expected, now.timestamp_millis(), effective_outcome == RequestTerminalOutcome::Completed),
                TerminalAuthority::Revocation { generation, expiry: expected } =>
                    authorize_execution_revocation(observed,
                        LeaseObservation { generation, deadline_ms: DateTime::parse_from_rfc3339(expected)?.timestamp_millis(), ..observed },
                        fresh_generation.as_str(), effective_outcome.request_state()),
            };
            if !authorized || row.terminal_output.is_some() {
                return Ok(TerminalizeResult::Lost);
            }
            let agent = row.agent_did.as_deref().context("missing request agent")?;
            let session_id = row.session_id.as_deref().context("missing request session")?;
            let headers = session::load_request_headers_in_txn(
                txn, session_id, agent, row.requester_did.as_deref(), request_doc_id
            ).await?;
            let eligible = |header: &&session::canonical_rows::TranscriptMessageRow| {
                header.message.role == MessageRole::Assistant &&
                matches!(header.message.publication, MessagePublication::RequestExecution { .. }
                    | MessagePublication::RequestRecovery { .. })
            };
            let selected;
            let selection = match selection.as_ref() {
                Some(explicit) => explicit,
                None => {
                    anyhow::ensure!(matches!(authority, TerminalAuthority::Revocation { .. }),
                        "only revocation may select output by metadata");
                    let latest = headers.iter().filter(eligible)
                        .max_by_key(|header| header.message.sequence);
                    if let Some(latest) = latest {
                        anyhow::ensure!(headers.iter().filter(eligible)
                            .filter(|header| header.message.sequence == latest.message.sequence).count() == 1,
                            "revocation terminal header sequence is ambiguous");
                    }
                    selected = latest.map_or(TerminalOutput::NoMessage, |header|
                        TerminalOutput::Message { message_doc_id: header.doc_id.clone() });
                    &selected
                }
            };
            match selection {
                TerminalOutput::Message { message_doc_id } => {
                    let matching = headers.iter().filter(eligible)
                        .filter(|header| &header.doc_id == message_doc_id).collect::<Vec<_>>();
                    anyhow::ensure!(matching.len() == 1, "terminal selection lacks exact owned assistant header");
                    if !matches!(authority, TerminalAuthority::Revocation { .. }) {
                        session::load_canonical_message_in_txn(
                            txn, message_doc_id, agent, row.requester_did.as_deref()
                        ).await?;
                    }
                }
                TerminalOutput::NoMessage => {
                    for header in headers.iter().filter(eligible) {
                        anyhow::ensure!(!matches!(authority, TerminalAuthority::Revocation { .. }),
                            "NoMessage cannot replace an owned assistant header during revocation");
                        match session::load_canonical_message_in_txn(
                            txn, &header.doc_id, agent, row.requester_did.as_deref()
                        ).await {
                            Ok(_) => anyhow::bail!("NoMessage cannot replace a reconstructable owned assistant header"),
                            // Match eligibleOwnedAssistantExists: invalid output
                            // is retained, but does not become terminal payload.
                            Err(error) if error.downcast_ref::<gents_protocol::output::ReconstructionError>().is_some() => {},
                            // Storage/authorization failures are not evidence of
                            // absence and must roll back the terminal decision.
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
            let reason = escape_graphql_string(reason);
            let target = escape_graphql_string(effective_outcome.request_state().as_str());
            let state = escape_graphql_string(state.as_str());
            let mutation = format!(r#"mutation($terminal_output: JSON) {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
                input: {{ lifecycle_state: "{target}", execution_generation: "{next_generation}",
                    execution_lease_expires_at: "{timestamp_gql}", failure_reason: "{reason}",
                    terminalized_at: "{timestamp_gql}", terminal_redrive_attempts: 0,
                    terminal_output: $terminal_output }}
            ) {{ _docID }} }}"#);
            let result = txn.execute_with_variables(&mutation, &serde_json::json!({
                "terminal_output": selection
            })).await?;
            if !result.get("data").and_then(|value| value.get("update_AgentRequest"))
                .is_some_and(response_has_documents) {
                return Ok(TerminalizeResult::Lost);
            }
            // After the winning request CAS, but in the SAME transaction.
            // Any missing reply, lost tool CAS or validation error rolls it all back.
            super::terminal_tools::account_tools_in_txn(
                txn, &row, &headers, owner,
                effective_outcome == RequestTerminalOutcome::Completed, &timestamp,
            ).await?;
            session::refresh_session_request_observation_in_txn(
                txn, agent, row.requester_did.as_deref(), session_id,
                request_doc_id, &row.request_id, &timestamp,
            ).await?;
            Ok(TerminalizeResult::Won)
        }),
    ).await
}

#[cfg(test)]
mod tests;
