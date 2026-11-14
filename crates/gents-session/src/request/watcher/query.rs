use rapidhash::{HashSetExt, RapidHashSet};

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{
    AgentRequest, AgentRequestRow, AgentRequestRowError, AGENT_REQUEST_FIELDS,
};

use super::DefraWatcher;

mod rows;
mod scoped;
use rows::{is_aged_background_completion_wakeup, is_pending, preclaim_signal, PreclaimSignal};
use scoped::{merge_scoped_scans, ActorScan, ScopedScanCause, ScopedScanFailure};

#[derive(Debug, thiserror::Error)]
pub enum WatcherScanError {
    #[error("request scan for {actor} failed: {source}")]
    Query {
        actor: String,
        #[source]
        source: gents_store::config_client::ConfigClientError,
    },
    #[error("request scan for {actor} returned invalid rows: {reason}")]
    Rows { actor: String, reason: String },
    #[error("request arrival scan for {actor} failed: {reason}")]
    Arrival { actor: String, reason: String },
    #[error("request scan for {actor} was not observed")]
    Unobserved { actor: String },
}

impl WatcherScanError {
    pub fn actor(&self) -> &str {
        match self {
            Self::Query { actor, .. }
            | Self::Rows { actor, .. }
            | Self::Arrival { actor, .. }
            | Self::Unobserved { actor } => actor,
        }
    }
}

fn scan_failure(failure: ScopedScanFailure<WatcherScanError>) -> WatcherScanError {
    match failure.cause {
        ScopedScanCause::MissingScan => WatcherScanError::Unobserved {
            actor: failure.actor,
        },
        ScopedScanCause::Failed(error) => error,
    }
}

#[derive(Debug)]
pub struct PendingRequestScan {
    pub requests: Vec<AgentRequest>,
    pub failures: Vec<WatcherScanError>,
}

impl PendingRequestScan {
    pub fn needs_rescan(&self) -> bool {
        scoped::needs_rescan(&self.failures)
    }
}

impl DefraWatcher {
    pub async fn try_fetch_request(&self, doc_id: &str) -> anyhow::Result<Option<AgentRequest>> {
        Ok(self
            .pending_requests()
            .await?
            .into_iter()
            .find(|request| request.doc_id == doc_id))
    }

    pub async fn pending_requests(&self) -> anyhow::Result<Vec<AgentRequest>> {
        let PendingRequestScan { requests, failures } = self.scan_pending_requests().await?;
        match failures.into_iter().next() {
            Some(failure) if requests.is_empty() => Err(failure.into()),
            _ => Ok(requests),
        }
    }

    pub async fn scan_pending_requests(&self) -> anyhow::Result<PendingRequestScan> {
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        let node_did = gents_store::graphql::escape_graphql_string(&self.node_did);
        let mut scans = Vec::with_capacity(self.agents.len());
        let mut positions = rapidhash::RapidHashMap::default();
        let mut has_parked = false;
        let mut wait_metadata = rapidhash::RapidHashMap::default();
        for identity in &self.agents {
            let actor = identity::Did::new(identity.did())?;
            let access = gents_store::config_client::ConfigAccess::local_managed_agent(
                self.node.clone(),
                actor.clone(),
                identity.as_ref(),
            )?;
            if let Err(error) =
                crate::request::lifecycle::recover_delegated_client_request_relationships_with_access(
                    &access,
                    &self.node_did,
                    &actor,
                )
                .await
            {
                tracing::warn!(agent_did = %actor, %error, "delegated request relationship recovery failed");
            }
            let actor_name = actor.as_str().to_owned();
            let agent_filter = format!(
                "agent_did: {{ _eq: \"{}\" }},",
                gents_store::graphql::escape_graphql_string(actor.as_str())
            );
            let query = format!(
                r#"{{
                AgentRequest(
                    filter: {{
                        node_did: {{ _eq: "{node_did}" }},
                        {agent_filter}
                        lifecycle_state: {{ _in: {active_runtime_states} }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{{fields}
                    lifecycle_state
                    interrupt_requested_at
                    valid_until
                }}
            }}"#,
                node_did = node_did,
                active_runtime_states = active_runtime_states,
                fields = AGENT_REQUEST_FIELDS,
            );

            let response = match access.execute(&query).await {
                Ok(response) => response,
                Err(source) => {
                    let error = WatcherScanError::Query {
                        actor: actor_name,
                        source,
                    };
                    tracing::warn!(%error, "agent request scan failed");
                    scans.push(ActorScan {
                        actor: identity.did().to_owned(),
                        outcome: Err(error),
                    });
                    continue;
                }
            };
            let (observed, malformed) = match parse_active_runtime_rows(response.get("data")) {
                Ok(rows) => rows,
                Err(error) => {
                    let error = WatcherScanError::Rows {
                        actor: actor_name,
                        reason: format!("{error:#}"),
                    };
                    tracing::warn!(%error, "agent request scan failed");
                    scans.push(ActorScan {
                        actor: identity.did().to_owned(),
                        outcome: Err(error),
                    });
                    continue;
                }
            };
            has_parked |= observed
                .iter()
                .any(|row| row.lifecycle_state == Some(RequestLifecycleState::WaitingForBackend));
            for row in malformed {
                self.terminalize_malformed_pending_request(&row).await;
            }
            self.maintain_parked_requests(&access, &observed).await;
            match load_backend_waits(&access, &observed).await {
                Ok(waits) => wait_metadata.extend(waits),
                Err(error) => {
                    tracing::warn!(agent_did = %actor, %error, "could not load backend wait metadata");
                }
            }
            for row in &observed {
                if is_pending(row) {
                    if let Err(error) = AgentRequest::try_from(row.clone()) {
                        self.terminalize_incoherent_pending_request(row, &error)
                            .await;
                    }
                }
            }
            let doc_ids = observed
                .iter()
                .filter_map(|row| row.doc_id.clone())
                .collect::<Vec<_>>();
            let order = gents_store::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                Some(actor.clone()),
                "watcher.request_arrival_positions",
                |txn| {
                    let doc_ids = &doc_ids;
                    Box::pin(async move {
                        crate::request::lifecycle::arrival_order::request_arrival_positions(
                            txn, doc_ids,
                        )
                        .await
                    })
                },
            )
            .await;
            let order = match order {
                Ok(order) => order,
                Err(error) => {
                    let error = WatcherScanError::Arrival {
                        actor: actor_name,
                        reason: format!("{error:#}"),
                    };
                    tracing::warn!(%error, "agent request scan failed");
                    scans.push(ActorScan {
                        actor: identity.did().to_owned(),
                        outcome: Err(error),
                    });
                    continue;
                }
            };
            positions.extend(order);
            scans.push(ActorScan {
                actor: identity.did().to_owned(),
                outcome: Ok(observed),
            });
        }
        let merged = merge_scoped_scans(
            self.agents.iter().map(|identity| identity.did()),
            scans,
            &positions,
        );
        let rows = merged.rows;
        let failures = merged.failures.into_iter().map(scan_failure).collect();
        self.has_parked
            .store(has_parked, std::sync::atomic::Ordering::Relaxed);
        let now = chrono::Utc::now();
        let mut candidates = self.due_parked_rows(&rows, &wait_metadata, now).await;
        candidates.extend(claimable_pending_rows_from_rows(rows));
        let requests = prioritize_aged_background_wakes(candidates, now)
            .into_iter()
            .map(AgentRequest::try_from)
            .collect::<Result<Vec<_>, AgentRequestRowError>>()?;
        Ok(PendingRequestScan { requests, failures })
    }

    /// Recovery of a parked request acts as the Agent that owns it: an expired
    /// resume returns to the wait, and an interrupted or expired wait settles.
    async fn maintain_parked_requests(
        &self,
        access: &gents_store::config_client::ConfigAccess,
        rows: &[AgentRequestRow],
    ) {
        let now = chrono::Utc::now();
        for row in rows {
            let (Some(doc), Some(generation)) =
                (row.doc_id.as_deref(), row.execution_generation.as_deref())
            else {
                continue;
            };
            match row.lifecycle_state {
                Some(RequestLifecycleState::ResumingBackend) => {
                    let Some(expiry) = row.execution_lease_expires_at.as_deref() else {
                        continue;
                    };
                    if chrono::DateTime::parse_from_rfc3339(expiry)
                        .ok()
                        .is_none_or(|time| time.with_timezone(&chrono::Utc) > now)
                    {
                        continue;
                    }
                    if let Err(error) = crate::request::lifecycle::repark_expired_resume(
                        access, doc, generation, expiry,
                    )
                    .await
                    {
                        tracing::warn!(request_doc_id = %doc, %error, "could not recover expired backend resume");
                    }
                }
                Some(RequestLifecycleState::WaitingForBackend) => {
                    let interrupted = row
                        .interrupt_requested_at
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty());
                    let expired = row
                        .deadline
                        .as_deref()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .is_some_and(|deadline| now >= deadline.with_timezone(&chrono::Utc));
                    if !interrupted && !expired {
                        continue;
                    }
                    if let Err(error) = crate::request::lifecycle::terminalize_parked_request(
                        access,
                        doc,
                        generation,
                        interrupted,
                    )
                    .await
                    {
                        tracing::warn!(request_doc_id = %doc, %error, "could not settle parked request");
                    }
                }
                _ => {}
            }
        }
    }

    /// Parked requests whose fallback is due or whose backend has a healthy
    /// observation newer than the wait; also schedules the next timed rescan.
    async fn due_parked_rows(
        &self,
        rows: &[AgentRequestRow],
        wait_metadata: &rapidhash::RapidHashMap<String, (Option<String>, Option<String>)>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<AgentRequestRow> {
        let mut parked = Vec::new();
        let mut next_wait_ms = super::GOSSIP_FALLBACK_POLL.as_millis() as u64;
        for row in rows
            .iter()
            .filter(|row| row.lifecycle_state == Some(RequestLifecycleState::WaitingForBackend))
        {
            let parse_time = |value: Option<&str>| {
                value
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|time| time.with_timezone(&chrono::Utc))
            };
            let Some(doc) = row.doc_id.as_deref() else {
                continue;
            };
            let Some((retry, started)) = wait_metadata.get(doc) else {
                continue;
            };
            let (Some(deadline), Some(retry), Some(started), Some(backend_id)) = (
                parse_time(row.deadline.as_deref()),
                parse_time(retry.as_deref()),
                parse_time(started.as_deref()),
                row.backend_id.as_deref().filter(|id| !id.is_empty()),
            ) else {
                continue;
            };
            for time in [deadline, retry] {
                if time > now {
                    let millis = (time - now).num_milliseconds().max(1) as u64;
                    next_wait_ms = next_wait_ms.min(millis);
                }
            }
            if deadline <= now
                || row
                    .interrupt_requested_at
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
            {
                continue;
            }
            let other_active_in_session = !row
                .purpose
                .is_some_and(gents_protocol::request_admission::RequestPurpose::is_auxiliary)
                && rows.iter().any(|other| {
                    other.doc_id.as_deref() != Some(doc)
                        && other.session_id == row.session_id
                        && !is_pending(other)
                        && !other.purpose.is_some_and(
                            gents_protocol::request_admission::RequestPurpose::is_auxiliary,
                        )
                });
            if other_active_in_session {
                continue;
            }
            let healthy = if retry > now {
                match &self.backend_health {
                    Some(health) => match health
                        .healthy_observed_at(&self.node, &self.node_did, backend_id)
                        .await
                    {
                        Ok(observed) => {
                            crate::request::lifecycle::fresh_healthy(started, now, observed)
                        }
                        Err(error) => {
                            tracing::warn!(%backend_id, %error, "could not inspect provider health for parked request");
                            false
                        }
                    },
                    None => false,
                }
            } else {
                false
            };
            if (retry <= now || healthy) && AgentRequest::try_from(row.clone()).is_ok() {
                parked.push(row.clone());
            }
        }
        self.parked_wait_ms
            .store(next_wait_ms, std::sync::atomic::Ordering::Relaxed);
        parked
    }

    fn managed_agent_for_rejection(
        &self,
        did: &str,
    ) -> Option<&dyn gents_store::identity::NodeIdentity> {
        self.agents
            .iter()
            .find(|identity| identity.did() == did)
            .map(|identity| identity.as_ref())
    }

    async fn terminalize_incoherent_pending_request(
        &self,
        row: &AgentRequestRow,
        error: &AgentRequestRowError,
    ) {
        let Some(agent_did) = row.agent_did.as_deref().filter(|did| !did.is_empty()) else {
            tracing::warn!(
                doc_id = row.doc_id.as_deref().unwrap_or_default(),
                "incoherent AgentRequest has no selected agent; leaving it pending"
            );
            return;
        };
        let Some(identity) = self.managed_agent_for_rejection(agent_did) else {
            tracing::warn!(
                agent_did,
                "incoherent request Agent signing capability unavailable; leaving it pending"
            );
            return;
        };
        let failure_reason =
            format!("request rejected at ingest: incoherent durable lineage ({error})");
        match crate::request::admission::terminalize_pending_request_rejection(
            &self.node,
            row.doc_id.as_deref().unwrap_or_default(),
            &self.node_did,
            agent_did,
            identity,
            &failure_reason,
            "terminalize_incoherent_pending_request",
        )
        .await
        {
            Err(persist_error) => tracing::error!(
                doc_id = row.doc_id.as_deref().unwrap_or_default(),
                request_id = %row.request_id,
                error = %gents_protocol::error_chain::ErrorChain(&persist_error),
                "failed to terminalize incoherent AgentRequest",
            ),
            Ok(crate::request::admission::PendingRejectionOutcome::NotMatched) => {}
            Ok(crate::request::admission::PendingRejectionOutcome::Terminalized) => tracing::warn!(
                doc_id = row.doc_id.as_deref().unwrap_or_default(),
                request_id = %row.request_id,
                %error,
                "terminalized incoherent AgentRequest at watcher ingest",
            ),
        }
    }

    async fn terminalize_malformed_pending_request(&self, row: &MalformedPendingRow) {
        if row.node_did != self.node_did
            || RequestLifecycleState::parse_opt(row.lifecycle_state.as_deref())
                != Some(RequestLifecycleState::Pending)
        {
            return;
        }
        let Some(agent_did) = row.agent_did.as_deref().filter(|did| !did.is_empty()) else {
            tracing::warn!(doc_id = %row.doc_id,
                "malformed AgentRequest has no selected agent; leaving it pending");
            return;
        };
        let Some(identity) = self.managed_agent_for_rejection(agent_did) else {
            tracing::warn!(
                agent_did,
                "malformed request Agent signing capability unavailable; leaving it pending"
            );
            return;
        };
        let reason = "request rejected at ingest: malformed durable AgentRequest row";
        if let Err(error) = crate::request::admission::terminalize_pending_request_rejection(
            &self.node,
            &row.doc_id,
            &self.node_did,
            agent_did,
            identity,
            reason,
            "terminalize_malformed_pending_request",
        )
        .await
        {
            tracing::error!(doc_id = %row.doc_id, error = %gents_protocol::error_chain::ErrorChain(&error),
                "failed to terminalize malformed AgentRequest");
        }
    }
}

/// Wait metadata is owned by the Agent that parked the request, so it is read
/// through that Agent's access.
async fn load_backend_waits(
    access: &gents_store::config_client::ConfigAccess,
    rows: &[AgentRequestRow],
) -> anyhow::Result<Vec<(String, (Option<String>, Option<String>))>> {
    let parked_ids: Vec<_> = rows
        .iter()
        .filter(|row| row.lifecycle_state == Some(RequestLifecycleState::WaitingForBackend))
        .filter_map(|row| row.doc_id.as_deref())
        .map(|doc| format!("\"{}\"", gents_store::graphql::escape_graphql_string(doc)))
        .collect();
    if parked_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = parked_ids.join(", ");
    let response = access
        .execute(&format!(
            r#"{{ AgentRequestBackendWait(filter: {{ request_doc_id: {{ _in: [{ids}] }} }}) {{ request_doc_id retry_at wait_started_at }} }}"#
        ))
        .await?;
    let string = |row: &serde_json::Value, name: &str| {
        row.get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    Ok(response
        .pointer("/data/AgentRequestBackendWait")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("backend wait metadata query omitted rows"))?
        .iter()
        .filter_map(|row| {
            Some((
                string(row, "request_doc_id")?,
                (string(row, "retry_at"), string(row, "wait_started_at")),
            ))
        })
        .collect())
}

#[cfg(test)]
fn active_runtime_rows(data: Option<&serde_json::Value>) -> anyhow::Result<Vec<AgentRequestRow>> {
    match data.and_then(|d| d.get("AgentRequest")) {
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Vec::new()),
    }
}

#[derive(Debug)]
struct MalformedPendingRow {
    doc_id: String,
    node_did: String,
    lifecycle_state: Option<String>,
    agent_did: Option<String>,
}

fn parse_active_runtime_rows(
    data: Option<&serde_json::Value>,
) -> anyhow::Result<(Vec<AgentRequestRow>, Vec<MalformedPendingRow>)> {
    let value = data
        .and_then(|data| data.get("AgentRequest"))
        .ok_or_else(|| anyhow::anyhow!("request watcher query omitted AgentRequest rows"))?;
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("AgentRequest query result is not an array"))?;
    let mut rows = Vec::with_capacity(values.len());
    let mut malformed = Vec::new();
    for value in values {
        match serde_json::from_value::<AgentRequestRow>(value.clone()) {
            Ok(row) => rows.push(row),
            Err(error) => {
                let string = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
                if let (Some(doc_id), Some(node_did)) = (string("_docID"), string("node_did")) {
                    malformed.push(MalformedPendingRow {
                        doc_id: doc_id.to_string(),
                        node_did: node_did.to_string(),
                        lifecycle_state: string("lifecycle_state").map(str::to_string),
                        agent_did: string("agent_did").map(str::to_string),
                    });
                } else {
                    tracing::warn!(%error, "unattributable malformed AgentRequest row quarantined");
                }
            }
        }
    }
    Ok((rows, malformed))
}

pub fn agent_request_from_mutation_response(
    response: &defra_node::QueryResponse,
    field: &str,
) -> anyhow::Result<Option<AgentRequest>> {
    Ok(
        gents_store::graphql::single_mutation_document(response, field)?
            .cloned()
            .map(serde_json::from_value::<AgentRequestRow>)
            .transpose()?
            .map(AgentRequest::try_from)
            .transpose()?,
    )
}

fn prioritize_aged_background_wakes(
    mut rows: Vec<AgentRequestRow>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AgentRequestRow> {
    rows.sort_by_key(|row| !is_aged_background_completion_wakeup(row, now));
    rows
}

/// Reviews borrow the live caller's token ledger. Replaying a pending review
/// would lose that ownership; expired and interrupted reviews still settle
/// through the ordinary preclaim owner.
fn replays_pending_request(purpose: gents_protocol::request_admission::RequestPurpose) -> bool {
    purpose != gents_protocol::request_admission::RequestPurpose::AccessReview
}

fn claimable_pending_rows_from_rows(rows: Vec<AgentRequestRow>) -> Vec<AgentRequestRow> {
    let blocked_sessions = rows
        .iter()
        .filter(|row| {
            !row.purpose
                .is_some_and(gents_protocol::request_admission::RequestPurpose::is_auxiliary)
        })
        .filter(|row| !is_pending(row))
        .filter_map(|row| row.session_id.clone())
        .collect::<RapidHashSet<_>>();
    let rows = rows
        .into_iter()
        .filter_map(|row| match AgentRequest::try_from(row.clone()) {
            Ok(_) => Some(row),
            Err(error) => {
                tracing::warn!(
                    doc_id = row.doc_id.as_deref().unwrap_or_default(),
                    request_id = %row.request_id,
                    %error,
                    "watcher quarantined incoherent AgentRequest row during pending scan",
                );
                None
            }
        })
        .collect::<Vec<_>>();
    let mut seen_pending_sessions = RapidHashSet::new();
    let mut claimable = Vec::new();

    for row in rows {
        let row_is_pending = is_pending(&row);
        let session_id = row.session_id.as_deref().unwrap_or_default();
        let pending_session_seen = seen_pending_sessions.contains(session_id);
        let session_blocked = blocked_sessions.contains(session_id);

        if row_is_pending {
            let auxiliary = row
                .purpose
                .is_some_and(gents_protocol::request_admission::RequestPurpose::is_auxiliary);
            match preclaim_signal(&row) {
                PreclaimSignal::Terminal => claimable.push(row.clone()),
                PreclaimSignal::Malformed => {
                    tracing::warn!(
                        doc_id = row.doc_id.as_deref().unwrap_or_default(),
                        request_id = %row.request_id,
                        "watcher refusing to claim AgentRequest with malformed valid_until"
                    );
                }
                PreclaimSignal::None => {
                    if row.purpose.is_some_and(replays_pending_request)
                        && (auxiliary || (!session_blocked && !pending_session_seen))
                    {
                        claimable.push(row.clone());
                    }
                }
            }
            if !auxiliary {
                seen_pending_sessions.insert(session_id.to_string());
            }
        }
    }

    claimable
}

#[cfg(test)]
mod tests {
    use super::{
        active_runtime_rows, claimable_pending_rows_from_rows, prioritize_aged_background_wakes,
    };

    fn versioned_wake_input(session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "queue": {
                "source": "background_completion",
                "policy": "coalesce",
                "key": format!("background_completion:{session_id}"),
                "queued_after_request_id": "parent",
                "background_completion_wake_version": 1
            }
        })
    }

    fn pending_row(
        request_id: &str,
        session_id: &str,
        created_at: &str,
        input: Option<serde_json::Value>,
        execution_origin: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "_docID": format!("doc-{request_id}"),
            "request_id": request_id,
            "purpose": "normal",
            "node_did": "did:agent:1",
            "agent_did": "default",
            "session_id": session_id,
            "content": "work",
            "input": input,
            "execution_origin": execution_origin,
            "created_at": created_at,
            "lifecycle_state": "pending"
        })
    }

    #[test]
    fn aged_completion_wake_moves_ahead_of_older_descendant() {
        let witness = gents_lean_vocab::lean_r6_backgrounding_case(
            "aged_background_wake_precedes_new_descendant",
        );
        assert!(witness.legal);
        assert_eq!(witness.reason.as_deref(), Some("aged_priority"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T22:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let data = serde_json::json!({
            "AgentRequest": [
                pending_row(
                    "older-descendant",
                    "descendant-session",
                    "2026-08-12T21:00:00Z",
                    None,
                    "interactive",
                ),
                pending_row(
                    "aged-wake",
                    "parent-session",
                    "2026-08-12T21:59:30Z",
                    Some(versioned_wake_input("parent-session")),
                    "scheduled",
                )
            ]
        });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        let ranked = prioritize_aged_background_wakes(rows, now);
        assert_eq!(ranked[0].request_id, "aged-wake");
        assert_eq!(ranked[1].request_id, "older-descendant");
    }

    #[test]
    fn fresh_completion_wake_preserves_fifo() {
        let witness =
            gents_lean_vocab::lean_r6_backgrounding_case("fresh_background_wake_preserves_fifo");
        assert!(!witness.legal);
        assert_eq!(witness.reason.as_deref(), Some("fifo"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T22:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let data = serde_json::json!({
            "AgentRequest": [
                pending_row(
                    "older-descendant",
                    "descendant-session",
                    "2026-08-12T21:59:00Z",
                    None,
                    "interactive",
                ),
                pending_row(
                    "fresh-wake",
                    "parent-session",
                    "2026-08-12T21:59:31Z",
                    Some(versioned_wake_input("parent-session")),
                    "scheduled",
                )
            ]
        });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        let ranked = prioritize_aged_background_wakes(rows, now);
        assert_eq!(ranked[0].request_id, "older-descendant");
        assert_eq!(ranked[1].request_id, "fresh-wake");
    }

    #[test]
    fn malformed_valid_until_is_never_claimable() {
        let mut row = pending_row(
            "malformed-ttl",
            "solo-session",
            "2026-08-12T21:00:00Z",
            None,
            "interactive",
        );
        row["valid_until"] = serde_json::json!("not-a-timestamp");
        let data = serde_json::json!({ "AgentRequest": [row] });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        assert!(
            rows.is_empty(),
            "malformed valid_until must never be claimable, got {} row(s)",
            rows.len()
        );
    }

    #[test]
    fn a_pending_review_keeps_its_live_caller_and_does_not_block_chat() {
        let mut review = pending_row(
            "review",
            "session",
            "2026-08-12T21:00:00Z",
            None,
            "interactive",
        );
        review["purpose"] = serde_json::json!("access-review");
        review["caused_by_parent_request_id"] = serde_json::json!("parent");
        review["caused_by_parent_request_doc_id"] = serde_json::json!("doc-parent");
        review["valid_until"] = serde_json::json!("2999-01-01T00:00:00Z");
        let normal = pending_row(
            "normal",
            "session",
            "2026-08-12T21:00:01Z",
            None,
            "interactive",
        );
        let data = serde_json::json!({ "AgentRequest": [review.clone(), normal] });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        assert_eq!(
            rows.iter()
                .map(|row| row.request_id.as_str())
                .collect::<Vec<_>>(),
            ["normal"]
        );
        review["valid_until"] = serde_json::json!("2000-01-01T00:00:00Z");
        let data = serde_json::json!({ "AgentRequest": [review] });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "review");
    }
}
