use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use super::*;
use crate::streaming::StreamWriter;

/// The lifecycle owns both authorization and renewal for durable response writes.
/// A stream buffer carries this fence, but cannot invent a renewal policy.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionWriteFence {
    pub(crate) request_doc_id: String,
    pub(crate) execution_generation: String,
    pub(crate) lease_duration_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionWriteKind {
    Begin,
    Progress,
    Observe,
}

impl ExecutionWriteFence {
    pub(crate) async fn execute_response_write(
        &self,
        node: &EmbeddedNode,
        response_mutation: &str,
        kind: ExecutionWriteKind,
    ) -> Result<defra_node::QueryResponse> {
        crate::retry::retry_terminal_persistence_operation(
            "owned_response_progress",
            crate::retry::TERMINAL_PERSISTENCE_MAX_RETRIES,
            std::time::Duration::from_millis(crate::retry::TERMINAL_PERSISTENCE_INITIAL_BACKOFF_MS),
            || async {
                let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
                let attempt = async {
                    let doc_id = escape_graphql_string(&self.request_doc_id);
                    let query = format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ request_id lifecycle_state execution_generation execution_lease_expires_at execution_progress_seq }} }}"#);
                    let result = txn.execute_local_response(&query).await?;
                    let row = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")?
                        .context("execution owner request disappeared")?;
                    let expiry = row.execution_lease_expires_at.as_deref().context("missing execution expiry")?;
                    let deadline = DateTime::parse_from_rfc3339(expiry)?.with_timezone(&Utc);
                    let now = Utc::now();
                    let new_deadline = (now + chrono::Duration::seconds(self.lease_duration_secs as i64))
                        .max(deadline + chrono::Duration::milliseconds(1));
                    let state = row.lifecycle_state.context("missing execution state")?;
                    let owner = row.execution_generation.as_deref().context("missing execution generation")?;
                    let response_query = format!(r#"{{ AgentResponse(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ _docID status content interrupted_at }} }}"#);
                    let response_result = txn.execute_local_response(&response_query).await?;
                    let response = crate::graphql::first_row::<ResponseLeaseView>(&response_result, "AgentResponse")?;
                    let operation = match kind {
                        ExecutionWriteKind::Begin => super::execution_policy::ExecutionOperation::Begin,
                        ExecutionWriteKind::Progress => super::execution_policy::ExecutionOperation::Progress { new_deadline: new_deadline.timestamp_millis() },
                        ExecutionWriteKind::Observe => super::execution_policy::ExecutionOperation::Observe,
                    };
                    anyhow::ensure!(super::execution_policy::authorize_live_execution(
                        super::execution_policy::ExecutionObservation {
                            request: state, response_streaming: response.as_ref().map(|v| v.status == "streaming"), generation: owner,
                            deadline: deadline.timestamp_millis(), progress_seq: u64::try_from(row.execution_progress_seq.context("missing execution progress")?)?,
                        }, &self.execution_generation, now.timestamp_millis(), operation
                    ), "stale or expired execution generation cannot write response");
                    let generation = escape_graphql_string(owner);
                    let expiry = escape_graphql_string(expiry);
                    let seq = row.execution_progress_seq.unwrap_or(0);
                    let input = if kind == ExecutionWriteKind::Progress {
                        format!(r#"execution_lease_expires_at: "{}", execution_progress_seq: {}"#,
                            new_deadline.to_rfc3339(), seq.checked_add(1).context("execution progress overflow")?)
                    } else if kind == ExecutionWriteKind::Begin {
                        format!(r#"lifecycle_state: "{}""#, RequestLifecycleState::Processing)
                    } else {
                        format!(r#"execution_generation: "{generation}""#)
                    };
                    let mutation = format!(r#"mutation {{ update_AgentRequest(
                        filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                            execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }},
                            execution_progress_seq: {{ _eq: {seq} }} }}, input: {{ {input} }}
                    ) {{ _docID }} }}"#);
                    let result = txn.execute_local_response(&mutation).await?;
                    anyhow::ensure!(result.data.as_ref().and_then(|v| v.get("update_AgentRequest")).is_some_and(response_has_documents),
                        "execution generation lost response write");
                    let result = txn.execute_local_response(response_mutation).await?;
                    anyhow::ensure!(result.data.as_ref().and_then(|v| v.get("update_AgentResponse").or_else(|| v.get("create_AgentResponse"))).is_some_and(response_has_documents)
                        || extract_single_doc_id(&result, "create_AgentResponse").is_some(),
                        "owned response write matched no document");
                    Ok::<_, anyhow::Error>(result)
                }.await;
                match attempt {
                    Ok(result) => txn.commit().await.map(|()| result),
                    Err(error) => { let _ = txn.discard().await; Err(error) }
                }
            }
        ).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionGeneration(String);

impl Drop for RequestLifecycle {
    fn drop(&mut self) {
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
            if let Err(error) = crate::graphql::graphql_mutation_with_transaction_retry(&node, &mutation, "relinquish_execution_lease").await {
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

    fn response_status(self) -> &'static str {
        match self {
            Self::Completed => "complete",
            Self::Failed | Self::Interrupted | Self::Dead | Self::Superseded => "error",
        }
    }

    fn conversation_status(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed | Self::Interrupted | Self::Dead | Self::Superseded => "active",
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

#[derive(Debug, serde::Deserialize)]
struct ResponseLeaseView {
    #[serde(rename = "_docID")]
    doc_id: String,
    status: String,
    #[serde(default)]
    content: String,
    interrupted_at: Option<String>,
}

enum TerminalAuthority<'a> {
    Owner(&'a str),
    Recovery {
        generation: &'a str,
        expiry: &'a str,
        progress: i64,
    },
    Revocation {
        generation: &'a str,
        expiry: &'a str,
        progress: i64,
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
            super::execution_policy::authorize_live_execution(
                super::execution_policy::ExecutionObservation {
                    request: row.lifecycle_state.context("missing execution state")?,
                    response_streaming: Some(true),
                    generation: row
                        .execution_generation
                        .as_deref()
                        .context("missing execution generation")?,
                    deadline: deadline.timestamp_millis(),
                    progress_seq: u64::try_from(
                        row.execution_progress_seq
                            .context("missing execution progress")?
                    )?,
                },
                self.execution_generation()?,
                Utc::now().timestamp_millis(),
                super::execution_policy::ExecutionOperation::Observe,
            ),
            "execution lease expired or ownership was revoked"
        );
        Ok(())
    }

    pub(crate) async fn terminalize_owned(
        &mut self,
        stream_writer: &crate::streaming::DefraStreamWriter,
        outcome: RequestTerminalOutcome,
        reason: Option<&str>,
    ) -> Result<TerminalizeResult> {
        if matches!(self.state, LocalLifecycleState::Streaming) {
            if let Some(doc_id) = self.response_doc_id.as_deref() {
                if let Err(error) = stream_writer.flush_pending(doc_id).await {
                    // A failed preview flush cannot turn completion into success,
                    // nor prevent failure from converging through the same owner.
                    let failed = if outcome == RequestTerminalOutcome::Completed {
                        RequestTerminalOutcome::Failed
                    } else {
                        outcome
                    };
                    let failure = format!("persisting final response preview: {error:#}");
                    let result = self
                        .terminalize_owned_without_stream(failed, reason.or(Some(&failure)))
                        .await?;
                    if let Some(doc_id) = self.response_doc_id.as_deref() {
                        stream_writer.discard_buffer(doc_id).await;
                    }
                    if outcome == RequestTerminalOutcome::Completed {
                        return Err(error.context("completion could not persist its final preview"));
                    }
                    return Ok(result);
                }
            }
        }
        let result = self
            .terminalize_owned_without_stream(outcome, reason)
            .await?;
        if let Some(doc_id) = self.response_doc_id.as_deref() {
            stream_writer.discard_buffer(doc_id).await;
        }
        Ok(result)
    }

    pub async fn terminalize_owned_without_stream(
        &mut self,
        outcome: RequestTerminalOutcome,
        reason: Option<&str>,
    ) -> Result<TerminalizeResult> {
        let generation = self.execution_generation()?.to_string();
        let result = terminalize_execution(
            &self.node,
            &self.request.doc_id,
            TerminalAuthority::Owner(&generation),
            outcome,
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

pub(crate) async fn recover_execution_generation(
    node: &EmbeddedNode,
    row: &AgentRequestRow,
    expected_generation: &str,
    expected_expiry: &str,
    expected_progress_seq: i64,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> Result<TerminalizeResult> {
    terminalize_execution(
        node,
        row.doc_id.as_deref().context("missing request document")?,
        TerminalAuthority::Recovery {
            generation: expected_generation,
            expiry: expected_expiry,
            progress: expected_progress_seq,
        },
        outcome,
        reason,
    )
    .await
}

/// LatestOnly and child deadlines revoke the observed execution through the
/// same atomic terminal owner. Unlike recovery, revocation may cancel a live lease.
pub(crate) async fn revoke_execution_generation(
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
            progress: row
                .execution_progress_seq
                .context("missing execution progress")?,
        },
        outcome,
        reason,
    )
    .await
}

async fn terminalize_execution(
    node: &EmbeddedNode,
    request_doc_id: &str,
    authority: TerminalAuthority<'_>,
    outcome: RequestTerminalOutcome,
    reason: &str,
) -> Result<TerminalizeResult> {
    use super::execution_policy::{
        authorize_execution_revocation, authorize_live_execution, ExecutionObservation,
        ExecutionOperation,
    };
    let fresh_generation = ExecutionGeneration::fresh();
    crate::retry::retry_terminal_persistence_operation(
        "terminalize_execution_generation", crate::retry::TERMINAL_PERSISTENCE_MAX_RETRIES,
        std::time::Duration::from_millis(crate::retry::TERMINAL_PERSISTENCE_INITIAL_BACKOFF_MS),
        || async {
            let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
            let attempt = async {
                let doc_id = escape_graphql_string(request_doc_id);
                let query = format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                    _docID request_id agent_did requester_did behavior_id session_id lifecycle_state
                    execution_generation execution_lease_expires_at execution_progress_seq interrupt_requested_at
                }} }}"#);
                let result = txn.execute_local_response(&query).await?;
                let row = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")?.context("execution request disappeared")?;
                let owner = row.execution_generation.as_deref().context("missing execution generation")?;
                let agent_did = escape_graphql_string(row.agent_did.as_deref().context("missing agent DID")?);
                let query = format!(r#"{{ AgentResponse(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }}, limit: 1) {{ _docID status content interrupted_at }} }}"#);
                let result = txn.execute_local_response(&query).await?;
                let response = crate::graphql::first_row::<ResponseLeaseView>(&result, "AgentResponse")?;
                let state = row.lifecycle_state.context("missing request state")?;
                let effective_outcome = if outcome == RequestTerminalOutcome::Failed
                    && row.interrupt_requested_at.as_deref().is_some_and(|v| !v.is_empty()) {
                    RequestTerminalOutcome::Interrupted
                } else { outcome };
                if state.is_terminal() {
                    return Ok(if matches!(authority, TerminalAuthority::Owner(expected) if expected == owner)
                        && state == effective_outcome.request_state()
                        && response.as_ref().is_some_and(|v| v.status == effective_outcome.response_status()) {
                        TerminalizeResult::AlreadySame
                    } else { TerminalizeResult::Lost });
                }
                let expiry = row.execution_lease_expires_at.as_deref().context("missing execution expiry")?;
                let deadline = DateTime::parse_from_rfc3339(expiry)?.with_timezone(&Utc);
                let progress = row.execution_progress_seq.context("missing execution progress")?;
                let now = Utc::now();
                let observed = ExecutionObservation { request: state,
                    response_streaming: response.as_ref().map(|v| v.status == "streaming"),
                    generation: owner, deadline: deadline.timestamp_millis(), progress_seq: u64::try_from(progress)? };
                let authorized = match &authority {
                    TerminalAuthority::Owner(expected) => authorize_live_execution(observed, expected, now.timestamp_millis(),
                        ExecutionOperation::Finalize { completed: effective_outcome == RequestTerminalOutcome::Completed }),
                    TerminalAuthority::Recovery { generation, expiry: expected_expiry, progress: expected_progress } => {
                        owner == *generation && expiry == *expected_expiry && progress == *expected_progress
                            && deadline < now && matches!(state, RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
                    },
                    TerminalAuthority::Revocation { generation, expiry: expected_expiry, progress: expected_progress } => {
                        let expected = ExecutionObservation { generation, deadline: DateTime::parse_from_rfc3339(expected_expiry)?.timestamp_millis(),
                            progress_seq: u64::try_from(*expected_progress)?, ..observed };
                        authorize_execution_revocation(observed, expected, fresh_generation.as_str(), effective_outcome.request_state())
                    }
                };
                if !authorized || response.as_ref().is_some_and(|v| v.status != "streaming" && v.status != effective_outcome.response_status()) {
                    return Ok(TerminalizeResult::Lost);
                }
                let terminal_generation = match authority { TerminalAuthority::Owner(_) => owner, _ => fresh_generation.as_str() };
                let generation = escape_graphql_string(owner);
                let next_generation = escape_graphql_string(terminal_generation);
                let expiry = escape_graphql_string(expiry);
                let timestamp = now.to_rfc3339();
                let reason_escaped = escape_graphql_string(reason);
                let target = effective_outcome.request_state();
                let mutation = format!(r#"mutation {{ update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                        execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }}, execution_progress_seq: {{ _eq: {progress} }} }},
                    input: {{ lifecycle_state: "{target}", execution_generation: "{next_generation}", execution_lease_expires_at: "{timestamp}",
                        failure_reason: "{reason_escaped}", terminalized_at: "{timestamp}", terminal_redrive_attempts: 0 }}
                ) {{ _docID }} }}"#);
                let result = txn.execute_local_response(&mutation).await?;
                if !result.data.as_ref().and_then(|v| v.get("update_AgentRequest")).is_some_and(response_has_documents) {
                    return Ok(TerminalizeResult::Lost);
                }
                let status = effective_outcome.response_status();
                let interrupted_at = if effective_outcome == RequestTerminalOutcome::Interrupted {
                    let at = response.as_ref().and_then(|v| v.interrupted_at.as_deref()).filter(|v| !v.is_empty()).unwrap_or(&timestamp);
                    format!(r#"interrupted_at: "{}","#, escape_graphql_string(at))
                } else { String::new() };
                let mutation = if let Some(response) = response.as_ref() {
                    let response_doc_id = escape_graphql_string(&response.doc_id);
                    let current_status = escape_graphql_string(&response.status);
                    let recovered_content = if matches!(authority, TerminalAuthority::Recovery { .. })
                        && response.status == "streaming" && effective_outcome != RequestTerminalOutcome::Completed {
                        let content = if response.content.trim().is_empty() { format!("Error: {reason}") }
                            else { format!("{}\n\n[Response interrupted — daemon restarted]", response.content) };
                        format!(r#"content: "{}","#, escape_graphql_string(&content))
                    } else { String::new() };
                    format!(r#"mutation {{ update_AgentResponse(filter: {{ _docID: {{ _eq: "{response_doc_id}" }}, status: {{ _eq: "{current_status}" }} }},
                        input: {{ {recovered_content} status: "{status}", error_message: "{reason_escaped}", {interrupted_at} completed_at: "{timestamp}" }}
                    ) {{ _docID }} }}"#)
                } else {
                    let request_id = escape_graphql_string(&row.request_id);
                    let behavior = escape_graphql_string(row.behavior_id.as_deref().unwrap_or_default());
                    let session = escape_graphql_string(row.session_id.as_deref().context("missing request session")?);
                    let requester = session::requester_did_create_field(row.requester_did.as_deref());
                    let content = escape_graphql_string(&format!("Error: {reason}"));
                    format!(r#"mutation {{ create_AgentResponse(input: {{ response_key: "{request_id}", request_id: "{request_id}", request_doc_id: "{doc_id}",
                        agent_did: "{agent_did}", {requester} behavior_id: "{behavior}", session_id: "{session}", content: "{content}", reasoning: "",
                        status: "{status}", error_message: "{reason_escaped}", token_count: 0, progress_seq: 0, reasoning_progress_seq: 0,
                        created_at: "{timestamp}", completed_at: "{timestamp}", {interrupted_at}
                    }}) {{ _docID }} }}"#)
                };
                let result = txn.execute_local_response(&mutation).await?;
                let key = if response.is_some() { "update_AgentResponse" } else { "create_AgentResponse" };
                anyhow::ensure!(result.data.as_ref().and_then(|v| v.get(key)).is_some_and(response_has_documents)
                    || extract_single_doc_id(&result, key).is_some(), "terminal response write matched no document");
                let projection = session::request_conversation_status_projection_mutation(
                    row.session_id.as_deref().context("missing request session")?, &row.request_id, effective_outcome.conversation_status(), &timestamp);
                txn.execute_local_response(&projection).await?;
                Ok::<_, anyhow::Error>(TerminalizeResult::Won)
            }.await;
            match attempt {
                Ok(TerminalizeResult::Won) => txn.commit().await.map(|()| TerminalizeResult::Won),
                Ok(result) => { let _ = txn.discard().await; Ok(result) },
                Err(error) => { let _ = txn.discard().await; Err(error) },
            }
        }
    ).await
}

#[cfg(test)]
mod tests;
