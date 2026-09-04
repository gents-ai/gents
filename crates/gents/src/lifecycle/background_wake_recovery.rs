use std::collections::BTreeSet;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::materialize::{
    build_signed_request, ParentLink, RequestIdentity, RequestSigner, RequestSpec, RetryLink,
    SamplingCarryover,
};
use crate::lifecycle::ExecutionOrigin;

use super::queue::{parse_queue_hints, QueuePolicy, QueueSource};
use super::{BackgroundWakeRedriveReport, RequestLifecycle};

const BACKGROUND_WAKE_REDRIVE_BATCH_LIMIT: usize = 64;
const BACKGROUND_WAKE_RETRY_BASE_SECONDS: i64 = 5;
const BACKGROUND_WAKE_RETRY_MAX_SECONDS: i64 = 60;

#[derive(Debug, Clone, Deserialize)]
struct ConversationRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    latest_request_id: String,
    updated_at: String,
    title: String,
    preview_text: String,
}

impl ConversationRow {
    fn rank(&self) -> (String, usize, String) {
        let richness = [
            self.title.trim(),
            self.preview_text.trim(),
            self.latest_request_id.trim(),
        ]
        .iter()
        .filter(|field| !field.is_empty())
        .count();
        (self.updated_at.clone(), richness, self.doc_id.clone())
    }
}

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
    /// coalesced `background_completion` queue metadata. The source must also
    /// remain the conversation's latest request and have retry budget left.
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
                let hints = parse_queue_hints(row.metadata.as_deref())?;
                automated_queue_key(&hints)
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
                        error = %error,
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
                    _docID request_id agent_did behavior_id session_id
                    retry_root_request temperature top_p top_k seed max_tokens
                    max_total_tokens metadata backend_id subagent_depth retry_count max_retries
                    terminalized_at
                }}
                successors: AgentRequest(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    retry_parent_request_doc_id: {{ _neq: null }}
                }}) {{ request_id retry_parent_request_doc_id }}
                pending: AgentRequest(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }}) {{ request_id session_id metadata }}
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

fn automated_queue_key(hints: &super::queue::QueueHints) -> Option<String> {
    (hints.source == QueueSource::BackgroundCompletion && hints.policy == QueuePolicy::Coalesce)
        .then(|| clean(hints.key.clone()))
        .flatten()
}

fn eligible_queue_key(candidate: &AgentRequestRow) -> Result<Option<String>> {
    let retry_count = required_i64(candidate.retry_count, "retry_count")?;
    let max_retries = required_i64(candidate.max_retries, "max_retries")?;
    if retry_count < 0 || retry_count >= max_retries {
        return Ok(None);
    }
    Ok(parse_queue_hints(candidate.metadata.as_deref())
        .and_then(|hints| automated_queue_key(&hints)))
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
    let mut last_error = None;
    for retry_index in 0..=crate::graphql::DEFRA_DB_CONFLICT_MAX_RETRIES {
        let txn = ConfigApplyTxn::begin_local(node, None).await?;
        let attempt = redrive_in_transaction(&txn, candidate, &request_id).await;
        let result = match attempt {
            Ok(outcome) => txn.commit().await.map(|()| outcome),
            Err(error) => {
                let _ = txn.discard().await;
                Err(error)
            }
        };
        match result {
            Ok(outcome) => return Ok(outcome),
            Err(error)
                if retry_index < crate::graphql::DEFRA_DB_CONFLICT_MAX_RETRIES
                    && retryable_transaction_error(&error) =>
            {
                let backoff = crate::graphql::defradb_conflict_retry_backoff(retry_index);
                last_error = Some(error);
                tokio::time::sleep(backoff).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("background wake redrive exhausted")))
}

fn retryable_transaction_error(error: &anyhow::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    crate::graphql::is_defradb_transaction_conflict_text(&text)
        || text.contains("unique")
        || text.contains("constraint")
        || text.contains("database is locked")
        || text.contains("compare-and-set lost")
}

async fn redrive_in_transaction(
    txn: &ConfigApplyTxn<'_>,
    candidate: &AgentRequestRow,
    request_id: &str,
) -> Result<RedriveOutcome> {
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
    if agent_request_rows(data, "source")?.len() != 1 {
        return Ok(RedriveOutcome::Ineligible);
    }
    let queue_key = eligible_queue_key(candidate)?.context("wake became ineligible")?;
    if agent_request_rows(data, "pending")?.iter().any(|row| {
        parse_queue_hints(row.metadata.as_deref())
            .and_then(|hints| automated_queue_key(&hints))
            .is_some_and(|key| key == queue_key)
    }) {
        return Ok(RedriveOutcome::Coalesced);
    }

    let mut conversations: Vec<ConversationRow> = serde_json::from_value(
        data.get("conversations")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .context("decoding background wake conversations")?;
    conversations.sort_by(|left, right| right.rank().cmp(&left.rank()));
    let Some(conversation) = conversations.first() else {
        return Ok(RedriveOutcome::Ineligible);
    };
    if conversation.latest_request_id != candidate.request_id {
        return Ok(RedriveOutcome::Ineligible);
    }

    let response = txn
        .execute(&redrive_mutation(candidate, conversation, request_id, &retry_key).await?)
        .await?;
    let data = response
        .get("data")
        .context("redrive mutation has no data")?;
    let created = data
        .get("successor")
        .is_some_and(crate::graphql::response_has_documents);
    let updated = data
        .get("conversation")
        .is_some_and(crate::graphql::response_has_documents);
    anyhow::ensure!(
        created && updated,
        "background wake redrive compare-and-set lost"
    );
    Ok(RedriveOutcome::Created {
        request_id: request_id.to_string(),
    })
}

fn agent_request_rows(data: &serde_json::Value, name: &str) -> Result<Vec<AgentRequestRow>> {
    serde_json::from_value(
        data.get(name)
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
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
            }}, limit: 1) {{ request_id _docID }}
            successor: AgentRequest(
                filter: {{ retry_key: {{ _eq: "{retry_key}" }} }}, limit: 1
            ) {{ request_id _docID }}
            pending: AgentRequest(filter: {{
                session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }},
                lifecycle_state: {{ _eq: "pending" }}
            }}) {{ request_id metadata }}
            conversations: AgentConversation(filter: {{
                session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }}
            }}) {{ _docID latest_request_id updated_at title preview_text }}
        }}"#
    ))
}

async fn redrive_mutation(
    candidate: &AgentRequestRow,
    conversation: &ConversationRow,
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
        requester_did: None,
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
        sampling: Some(SamplingCarryover {
            temperature: candidate.temperature,
            top_p: candidate.top_p,
            top_k: candidate.top_k,
            seed: candidate.seed,
            max_tokens: candidate.max_tokens,
            max_total_tokens: candidate.max_total_tokens,
            backend_id: candidate.backend_id.clone(),
        }),
        metadata: candidate.metadata.clone(),
        retry_key: Some(retry_key.to_string()),
        ..RequestSpec::new(identity, admission)
    };
    let create = build_signed_request(spec, RequestSigner::RegisteredTarget).await?;
    let request_fields = create.graphql_input_fields().map_err(anyhow::Error::msg)?;
    Ok(format!(
        r#"mutation {{
            successor: create_AgentRequest(input: {{
                {request_fields}
            }}) {{ _docID }}
            conversation: update_AgentConversation(
                filter: {{ _docID: {{ _eq: "{conversation_doc_id}" }},
                    latest_request_id: {{ _eq: "{source_id}" }} }},
                input: {{ latest_request_id: "{request_id}", preview_text: "{content}",
                    status: "active", updated_at: "{created_at}" }}
            ) {{ _docID }}
        }}"#,
        source_id = escape_graphql_string(&candidate.request_id),
        content = escape_graphql_string(BACKGROUND_COMPLETION_WAKE_PROMPT),
        created_at = escape_graphql_string(&now),
        conversation_doc_id = escape_graphql_string(&conversation.doc_id),
    ))
}

/// Pins today's `AgentRequestCreate::graphql_input_fields()` output for
/// `redrive_mutation` (#1336 Task 1), before it is switched onto
/// `build_signed_request` (#1336 Task 2).
///
/// `redrive_mutation` is private and returns one combined mutation string
/// (the successor `create_AgentRequest` plus a conversation `update`), not
/// the `AgentRequestCreate` it builds, and it stamps `created_at` from
/// `Utc::now()` internally. This reproduces its DTO-construction statements
/// verbatim (see `redrive_mutation` above) with a fixed `now` in place of
/// `Utc::now()`, so — with a deterministic signing identity — the whole
/// output, including the signature, is stable across runs.
#[cfg(test)]
mod pin_tests {
    use super::*;
    use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    #[tokio::test]
    async fn pin_redrive_mutation_dto_construction() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let candidate = AgentRequestRow {
            doc_id: Some("failed-doc-1".to_string()),
            request_id: "failed-request-1".to_string(),
            agent_did: Some(PIN_FIXED_DID.to_string()),
            behavior_id: Some("behavior-1".to_string()),
            session_id: Some("sess-redrive-1".to_string()),
            retry_root_request: Some("root-request-1".to_string()),
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: Some(40),
            seed: Some(7),
            max_tokens: Some(1024),
            max_total_tokens: Some(4096),
            metadata: Some(r#"{"queue":{"source":"scheduled"}}"#.to_string()),
            backend_id: Some("backend-1".to_string()),
            subagent_depth: Some(1),
            retry_count: Some(2),
            max_retries: Some(5),
            terminalized_at: Some("2029-12-31T23:59:59Z".to_string()),
            ..Default::default()
        };
        let request_id = "redrive-request-1".to_string();
        let retry_key = "redrive-retry-key-1".to_string();

        // Driven through `build_signed_request` with the equivalent
        // `RequestSpec` (a fixed `now` in place of `Utc::now()`), asserting
        // against the output pinned by reproducing `redrive_mutation`'s
        // DTO-construction statements directly.
        let now = "2030-01-01T00:00:00Z".to_string();
        let retry_root = candidate
            .retry_root_request
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| candidate.request_id.clone());
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
                candidate.agent_did.as_deref().unwrap(),
                &candidate.request_id,
            );
        let spec = crate::lifecycle::materialize::RequestSpec {
            identity: crate::lifecycle::materialize::RequestIdentity {
                requester_did: None,
                request_id: request_id.clone(),
                agent_did: candidate.agent_did.clone().unwrap(),
                behavior_id: candidate.behavior_id.clone().unwrap(),
                session_id: candidate.session_id.clone().unwrap(),
                content: BACKGROUND_COMPLETION_WAKE_PROMPT.to_string(),
                execution_origin: crate::lifecycle::ExecutionOrigin::Scheduled,
                created_at: now,
            },
            admission,
            initial_lifecycle_state:
                gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
            trigger_lineage: crate::lifecycle::TriggerLineage::default(),
            trigger_doc_id: None,
            workspace: None,
            subagent: Some(crate::lifecycle::materialize::ParentLink {
                depth: candidate
                    .subagent_depth
                    .map(u32::try_from)
                    .transpose()
                    .unwrap()
                    .unwrap_or_default(),
                parent_request_id: candidate.request_id.clone(),
                parent_request_doc_id: candidate.doc_id.clone().unwrap(),
                parent_tool_call_id: None,
                parent_tool_call_doc_id: None,
            }),
            retry: Some(crate::lifecycle::materialize::RetryLink {
                parent_request_id: Some(candidate.request_id.clone()),
                parent_request_doc_id: candidate.doc_id.clone(),
                root_request_id: retry_root,
                retry_count: candidate.retry_count.unwrap() + 1,
                max_retries: candidate.max_retries.unwrap(),
            }),
            sampling: Some(crate::lifecycle::materialize::SamplingCarryover {
                temperature: candidate.temperature,
                top_p: candidate.top_p,
                top_k: candidate.top_k,
                seed: candidate.seed,
                max_tokens: candidate.max_tokens,
                max_total_tokens: candidate.max_total_tokens,
                backend_id: candidate.backend_id.clone(),
            }),
            metadata: candidate.metadata.clone(),
            retry_key: Some(retry_key),
            valid_until: None,
        };
        let create = crate::lifecycle::materialize::build_signed_request(
            spec,
            crate::lifecycle::materialize::RequestSigner::RegisteredTarget,
        )
        .await
        .expect("sign redrive request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        assert_eq!(
            fields,
            "request_id: \"redrive-request-1\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"behavior-1\", session_id: \"sess-redrive-1\", retry_parent_request: \"failed-request-1\", retry_parent_request_doc_id: \"failed-doc-1\", retry_root_request: \"root-request-1\", retry_key: \"redrive-retry-key-1\", content: \"Review the new background completion results and continue the task if needed.\", temperature: 0.7, top_p: 0.9, top_k: 40, seed: 7, max_tokens: 1024, max_total_tokens: 4096, metadata: \"{\\\"queue\\\":{\\\"source\\\":\\\"scheduled\\\"}}\", backend_id: \"backend-1\", execution_origin: \"scheduled\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 3, max_retries: 5, subagent_depth: 1, caused_by_parent_request_id: \"failed-request-1\", caused_by_parent_request_doc_id: \"failed-doc-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"5PhWwYmgF63HgneAud3fR5z7BZiqeyDmV4K8qLffUj1SADPSGJ2JQoYKbeNZH4zTZDfF6zg4XG6vERcyeJi1jXNY\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"failed-request-1\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }
}
