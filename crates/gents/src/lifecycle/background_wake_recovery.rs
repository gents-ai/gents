use std::collections::BTreeSet;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;

use crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::materialize::{
    build_signed_request, ParentLink, RequestIdentity, RequestSigner, RequestSpec, RetryLink,
};
use crate::lifecycle::ExecutionOrigin;

use super::queue::{row_queue, RequestQueue};
use super::{BackgroundWakeRedriveReport, RequestLifecycle};

const BACKGROUND_WAKE_REDRIVE_BATCH_LIMIT: usize = 64;
const BACKGROUND_WAKE_RETRY_BASE_SECONDS: i64 = 5;
const BACKGROUND_WAKE_RETRY_MAX_SECONDS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RedriveOutcome {
    Created { request_id: String },
    AlreadyCreated,
    Coalesced,
    Ineligible,
}

impl RequestLifecycle {
    /// Create bounded retry successors for failed canonical background wakes.
    ///
    /// This is deliberately narrower than interactive request retry: the
    /// source must be a failed scheduled request carrying the versioned,
    /// coalesced `background_completion` queue input. The source must also
    /// remain the latest request across session requesters and have retry budget left.
    /// A unique per-source `retry_key` plus a private transaction makes the
    /// sweep idempotent across concurrent ticks and process restarts.
    pub async fn redrive_failed_background_wakeups(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<BackgroundWakeRedriveReport> {
        let (candidates, successors, pending) = load_candidates(node, agent_did).await?;
        let mut report = BackgroundWakeRedriveReport {
            scanned: candidates.len(),
            ..Default::default()
        };
        let successor_parents = successors
            .into_iter()
            .filter_map(|row: AgentRequestRow| clean(row.retry_parent_request_doc_id))
            .collect::<BTreeSet<_>>();
        let pending_keys = pending
            .into_iter()
            .filter_map(|row: AgentRequestRow| {
                automated_queue_key(row_queue(&row)?)
                    .and_then(|key| row.session_id.map(|session| (session, key)))
            })
            .collect::<BTreeSet<_>>();

        let mut eligible = Vec::new();
        for candidate in candidates {
            let Some(queue_key) = eligible_queue_key(&candidate)? else {
                report.ineligible += 1;
                continue;
            };
            if successor_parents.contains(required_str(candidate.doc_id.as_deref(), "_docID")?) {
                report.already_redriven += 1;
                continue;
            }
            if pending_keys.contains(&(
                required_str(candidate.session_id.as_deref(), "session_id")?.to_string(),
                queue_key,
            )) {
                report.coalesced += 1;
                continue;
            }
            if !retry_is_due(&candidate, chrono::Utc::now())? {
                report.deferred += 1;
                continue;
            }
            eligible.push(candidate);
        }

        for candidate in eligible
            .into_iter()
            .take(BACKGROUND_WAKE_REDRIVE_BATCH_LIMIT)
        {
            match redrive_one(node, &candidate).await {
                Ok(RedriveOutcome::Created { request_id }) => {
                    report.redriven += 1;
                    tracing::info!(
                        source_request_id = %candidate.request_id,
                        request_id,
                        session_id = required_str(candidate.session_id.as_deref(), "session_id")?,
                        retry_count = required_i64(candidate.retry_count, "retry_count")? + 1,
                        max_retries = required_i64(candidate.max_retries, "max_retries")?,
                        "redrove failed background-completion wake"
                    );
                }
                Ok(RedriveOutcome::AlreadyCreated) => report.already_redriven += 1,
                Ok(RedriveOutcome::Coalesced) => report.coalesced += 1,
                Ok(RedriveOutcome::Ineligible) => report.ineligible += 1,
                Err(error) => {
                    report.failed += 1;
                    tracing::warn!(
                        request_id = %candidate.request_id,
                        session_id = required_str(candidate.session_id.as_deref(), "session_id")?,
                        error = ?error,
                        "failed to redrive background-completion wake"
                    );
                }
            }
        }
        Ok(report)
    }
}

async fn load_candidates(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<(
    Vec<AgentRequestRow>,
    Vec<AgentRequestRow>,
    Vec<AgentRequestRow>,
)> {
    let agent_did = escape_graphql_string(agent_did);
    let response = node
        .execute(&format!(
            r#"{{
                failed: AgentRequest(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    lifecycle_state: {{ _eq: "failed" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}, order: [{{ terminalized_at: ASC }}, {{ request_id: ASC }}]) {{
                    _docID request_id agent_did requester_did behavior_id session_id
                    retry_root_request input
                    subagent_depth retry_count max_retries
                    terminalized_at
                }}
                successors: AgentRequest(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    retry_parent_request_doc_id: {{ _neq: null }}
                }}) {{ request_id retry_parent_request_doc_id }}
                pending: AgentRequest(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }}) {{ request_id session_id requester_did input }}
            }}"#
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!("querying failed background wakes: {:?}", response.errors);
    }
    let data = response.data.context("background wake query has no data")?;
    let failed: Vec<AgentRequestRow> =
        serde_json::from_value(data["failed"].clone()).context("decoding failed wakes")?;
    for candidate in &failed {
        validate_failed_wake(candidate)?;
    }
    Ok((
        failed,
        serde_json::from_value(data["successors"].clone()).context("decoding successors")?,
        serde_json::from_value(data["pending"].clone()).context("decoding pending wakes")?,
    ))
}

fn required_str<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value.ok_or_else(|| anyhow::anyhow!("failed background wake is missing {field}"))
}

fn required_i64(value: Option<i64>, field: &str) -> Result<i64> {
    value.ok_or_else(|| anyhow::anyhow!("failed background wake is missing {field}"))
}

fn validate_failed_wake(candidate: &AgentRequestRow) -> Result<()> {
    required_str(candidate.doc_id.as_deref(), "_docID")?;
    required_str(candidate.agent_did.as_deref(), "agent_did")?;
    required_str(candidate.behavior_id.as_deref(), "behavior_id")?;
    required_str(candidate.session_id.as_deref(), "session_id")?;
    required_i64(candidate.retry_count, "retry_count")?;
    required_i64(candidate.max_retries, "max_retries")?;
    candidate
        .subagent_depth
        .map(u32::try_from)
        .transpose()
        .context("failed background wake subagent_depth must fit in u32")?;
    Ok(())
}

fn clean(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

fn automated_queue_key(queue: &RequestQueue) -> Option<String> {
    (queue.source == gents_protocol::request_input::QueueSource::BackgroundCompletion
        && queue.policy == gents_protocol::request_input::QueuePolicy::Coalesce)
        .then(|| clean(queue.key.clone()))
        .flatten()
}

fn eligible_queue_key(candidate: &AgentRequestRow) -> Result<Option<String>> {
    let retry_count = required_i64(candidate.retry_count, "retry_count")?;
    let max_retries = required_i64(candidate.max_retries, "max_retries")?;
    if retry_count < 0 || retry_count >= max_retries {
        return Ok(None);
    }
    Ok(candidate
        .input
        .as_ref()
        .and_then(|input| input.queue.as_ref())
        .and_then(automated_queue_key))
}

pub fn background_wake_retry_delay(retry_count: i64) -> chrono::Duration {
    let exponent = u32::try_from(retry_count.max(0))
        .unwrap_or(u32::MAX)
        .min(30);
    let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
    chrono::Duration::seconds(
        BACKGROUND_WAKE_RETRY_BASE_SECONDS
            .saturating_mul(multiplier)
            .min(BACKGROUND_WAKE_RETRY_MAX_SECONDS),
    )
}

pub fn background_wake_next_retry_at(
    terminalized_at: Option<&str>,
    retry_count: i64,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let terminalized_at = chrono::DateTime::parse_from_rfc3339(terminalized_at?)
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(terminalized_at + background_wake_retry_delay(retry_count))
}

fn retry_is_due(candidate: &AgentRequestRow, now: chrono::DateTime<chrono::Utc>) -> Result<bool> {
    Ok(background_wake_next_retry_at(
        candidate.terminalized_at.as_deref(),
        required_i64(candidate.retry_count, "retry_count")?,
    )
    .is_none_or(|next_retry_at| next_retry_at <= now))
}

async fn redrive_one(node: &EmbeddedNode, candidate: &AgentRequestRow) -> Result<RedriveOutcome> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_id = &request_id;
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::Standard,
        "lifecycle.background_wake_redrive",
        move |txn| {
            Box::pin(async move { redrive_in_transaction(txn, candidate, request_id).await })
        },
    )
    .await
}

async fn redrive_in_transaction(
    txn: &ConfigApplyTxn<'_>,
    candidate: &AgentRequestRow,
    request_id: &str,
) -> Result<RedriveOutcome> {
    if crate::goal::load_canonical_goal_in_txn(
        txn,
        required_str(candidate.agent_did.as_deref(), "agent_did")?,
        required_str(candidate.session_id.as_deref(), "session_id")?,
    )
    .await?
    .is_some()
    {
        return Ok(RedriveOutcome::Ineligible);
    }
    let retry_key = format!(
        "retry:doc:{}",
        required_str(candidate.doc_id.as_deref(), "_docID")?
    );
    let response = txn
        .execute(&precondition_query(candidate, &retry_key)?)
        .await?;
    let data = response.get("data").context("redrive query has no data")?;
    if !agent_request_rows(data, "successor")?.is_empty() {
        return Ok(RedriveOutcome::AlreadyCreated);
    }
    let sources = agent_request_rows(data, "source")?;
    let Some(current) = sources.first().filter(|_| sources.len() == 1) else {
        return Ok(RedriveOutcome::Ineligible);
    };
    if current.request_id != candidate.request_id
        || current.session_id != candidate.session_id
        || current.requester_did != candidate.requester_did
        || eligible_queue_key(current)?.is_none()
    {
        return Ok(RedriveOutcome::Ineligible);
    }
    let candidate = current;
    let queue_key = eligible_queue_key(candidate)?.context("wake became ineligible")?;
    if agent_request_rows(data, "pending")?.iter().any(|row| {
        row_queue(row)
            .and_then(automated_queue_key)
            .is_some_and(|key| key == queue_key)
    }) {
        return Ok(RedriveOutcome::Coalesced);
    }

    let agent_did = required_str(candidate.agent_did.as_deref(), "agent_did")?;
    let session_id = required_str(candidate.session_id.as_deref(), "session_id")?;
    let source_doc_id = required_str(candidate.doc_id.as_deref(), "_docID")?;
    let Some(head) =
        crate::session::load_latest_request_in_txn(txn, agent_did, session_id, None).await?
    else {
        return Ok(RedriveOutcome::Ineligible);
    };
    if head.observed.request_doc_id != source_doc_id
        || head.observed.request_id != candidate.request_id
    {
        return Ok(RedriveOutcome::Ineligible);
    }
    let Some(_owner) = crate::session::load_agent_session_row_in_txn(
        txn,
        agent_did,
        session_id,
        candidate.requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(RedriveOutcome::Ineligible);
    };
    let response = txn
        .execute_local_response(&redrive_mutation(candidate, request_id, &retry_key).await?)
        .await?;
    let created = crate::watcher::agent_request_from_mutation_response(&response, "successor")?
        .context("background wake successor create matched no document")?;
    let successor = crate::session::load_latest_request_in_txn(txn, agent_did, session_id, None)
        .await?
        .context("background wake successor is missing")?;
    anyhow::ensure!(
        successor.observed.request_doc_id == created.doc_id
            && successor.observed.request_id == created.request_id,
        "background wake successor is not the canonical request head"
    );
    // Only advance the observation here. Reopening remains the claim owner's
    // responsibility, and the observation never supplies retry authority.
    anyhow::ensure!(
        crate::session::advance_session_request_observation_in_txn(
            txn,
            &successor,
            &created.content,
            &created.created_at,
        )
        .await?,
        "background wake successor observation was not advanced"
    );
    Ok(RedriveOutcome::Created {
        request_id: request_id.to_string(),
    })
}

fn agent_request_rows(data: &serde_json::Value, name: &str) -> Result<Vec<AgentRequestRow>> {
    serde_json::from_value(
        data.get(name)
            .cloned()
            .with_context(|| format!("query omitted {name}"))?,
    )
    .with_context(|| format!("decoding {name} AgentRequest rows"))
}

fn precondition_query(candidate: &AgentRequestRow, retry_key: &str) -> Result<String> {
    let doc_id = escape_graphql_string(required_str(candidate.doc_id.as_deref(), "_docID")?);
    let agent_did =
        escape_graphql_string(required_str(candidate.agent_did.as_deref(), "agent_did")?);
    let session_id =
        escape_graphql_string(required_str(candidate.session_id.as_deref(), "session_id")?);
    let retry_key = escape_graphql_string(retry_key);
    Ok(format!(
        r#"{{
            source: AgentRequest(filter: {{
                _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }},
                lifecycle_state: {{ _eq: "failed" }},
                execution_origin: {{ _eq: "scheduled" }}
            }}, limit: 2) {{ {request_fields} retry_count max_retries retry_root_request }}
            successor: AgentRequest(
                filter: {{ retry_key: {{ _eq: "{retry_key}" }} }}, limit: 1
            ) {{ request_id _docID }}
            pending: AgentRequest(filter: {{
                session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }},
                lifecycle_state: {{ _eq: "pending" }}
            }}) {{ request_id input }}
        }}"#,
        request_fields = crate::watcher::AGENT_REQUEST_FIELDS,
    ))
}

async fn redrive_mutation(
    candidate: &AgentRequestRow,
    request_id: &str,
    retry_key: &str,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let retry_root = candidate
        .retry_root_request
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&candidate.request_id);
    let agent_did = required_str(candidate.agent_did.as_deref(), "agent_did")?;
    let behavior_id = required_str(candidate.behavior_id.as_deref(), "behavior_id")?;
    let session_id = required_str(candidate.session_id.as_deref(), "session_id")?;
    let doc_id = required_str(candidate.doc_id.as_deref(), "_docID")?;
    let subagent_depth = candidate
        .subagent_depth
        .map(u32::try_from)
        .transpose()
        .context("failed background wake subagent_depth must fit in u32")?
        .unwrap_or_default();
    let retry_count = required_i64(candidate.retry_count, "retry_count")?;
    let max_retries = required_i64(candidate.max_retries, "max_retries")?;
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            agent_did,
            &candidate.request_id,
        );
    let parent_link = ParentLink {
        depth: subagent_depth,
        parent_request_id: candidate.request_id.clone(),
        parent_request_doc_id: doc_id.to_string(),
        ..Default::default()
    };
    let identity = RequestIdentity {
        requester_did: candidate.requester_did.clone(),
        request_id: request_id.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: behavior_id.to_string(),
        session_id: session_id.to_string(),
        content: BACKGROUND_COMPLETION_WAKE_PROMPT.to_string(),
        execution_origin: ExecutionOrigin::Scheduled,
        created_at: now.clone(),
    };
    let spec = RequestSpec {
        subagent: Some(parent_link),
        retry: Some(RetryLink {
            parent_request_id: Some(candidate.request_id.clone()),
            parent_request_doc_id: Some(doc_id.to_string()),
            root_request_id: retry_root.to_string(),
            retry_count: retry_count + 1,
            max_retries,
        }),
        input: candidate.input.clone().unwrap_or_default(),
        retry_key: Some(retry_key.to_string()),
        ..RequestSpec::new(identity, admission)
    };
    let create = build_signed_request(spec, RequestSigner::RegisteredTarget).await?;
    let request_fields = create.graphql_input_fields().map_err(anyhow::Error::msg)?;
    Ok(format!(
        r#"mutation {{
            successor: create_AgentRequest(input: {{ {request_fields} }}) {{ {fields} }}
        }}"#,
        fields = crate::watcher::AGENT_REQUEST_FIELDS,
    ))
}
