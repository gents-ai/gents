//! Explicit request liveness, independent of provider/tool reads and output.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;

use super::execution_policy::{authorize_renewal, is_live, renewable_lifecycle, LeaseObservation};
use crate::graphql::{escape_graphql_string, response_has_documents};

/// Owned by RequestLifecycle. Dropping the owner must not leave a heartbeat
/// task keeping abandoned work alive indefinitely.
pub(super) struct RenewalTask {
    task: tokio::task::JoinHandle<()>,
    lost: tokio_util::sync::CancellationToken,
}

impl Drop for RenewalTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RenewalTask {
    pub(super) fn start(
        node: Arc<EmbeddedNode>,
        request_doc_id: String,
        generation: String,
        duration_ms: u64,
    ) -> Self {
        let lost = tokio_util::sync::CancellationToken::new();
        let observed_lost = lost.clone();
        let task = tokio::spawn(async move {
            // Poll more often than renewal is due, but only the bounded policy
            // writes. Skip missed ticks after suspension; never catch up with
            // a burst of renewals or revive an expired generation.
            let poll_interval = renewal_poll_interval(duration_ms);
            let mut ticker = tokio::time::interval(poll_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let scheduled = ticker.tick().await;
                let delay = scheduled.elapsed();
                if delay >= poll_interval {
                    tracing::warn!(
                        %request_doc_id, %generation,
                        poll_delay_ms = delay.as_millis() as u64,
                        poll_interval_ms = poll_interval.as_millis() as u64,
                        lease_duration_ms = duration_ms,
                        "execution lease renewal poll was delayed"
                    );
                }
                let started = std::time::Instant::now();
                let outcome = renew_once(&node, &request_doc_id, &generation).await;
                let elapsed = started.elapsed();
                if elapsed >= poll_interval {
                    tracing::warn!(
                        %request_doc_id, %generation,
                        elapsed_ms = elapsed.as_millis() as u64,
                        poll_interval_ms = poll_interval.as_millis() as u64,
                        "execution lease renewal attempt was slow"
                    );
                }
                match outcome {
                    Ok(true) => {}
                    Ok(false) => {
                        observed_lost.cancel();
                        break;
                    }
                    Err(error) => {
                        // Do not invent liveness on errors. The next bounded
                        // poll rereads authoritative state; expiry remains final.
                        tracing::warn!(%error, %request_doc_id, %generation, "execution lease renewal failed");
                    }
                }
            }
        });
        Self { task, lost }
    }

    /// Cancelled once a renewal poll observes this generation no longer owns
    /// its request: revoked, superseded, terminal, or expired.
    pub(super) fn ownership_lost(&self) -> tokio_util::sync::CancellationToken {
        self.lost.clone()
    }
}

fn renewal_poll_interval(duration_ms: u64) -> Duration {
    Duration::from_millis((duration_ms / 4).max(1))
}

#[cfg(test)]
mod cadence_tests {
    use super::*;

    #[test]
    fn two_minute_default_polls_every_thirty_seconds() {
        let duration_ms = crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS * 1_000;
        assert_eq!(renewal_poll_interval(duration_ms), Duration::from_secs(30));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenewalAttemptOutcome {
    Committed,
    Skipped,
    Lost,
}

/// Return false when this generation is no longer an active owner. A true
/// result includes an early poll that performed no mutation.
async fn renew_once(node: &EmbeddedNode, request_doc_id: &str, generation: &str) -> Result<bool> {
    Ok(!matches!(
        renew_once_with_time(node, request_doc_id, generation, None, None).await?,
        RenewalAttemptOutcome::Lost
    ))
}

/// Explicit-time form used by model-driven conformance fixtures. Production
/// callers always enter through `renew_once`, so the wall-clock read remains
/// owned by this module and no process-global test clock can affect siblings.
#[cfg(test)]
pub(crate) async fn renew_once_at(
    node: &EmbeddedNode,
    request_doc_id: &str,
    generation: &str,
    expected_deadline: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<RenewalAttemptOutcome> {
    renew_once_with_time(
        node,
        request_doc_id,
        generation,
        Some(expected_deadline),
        Some(now),
    )
    .await
}

#[cfg(test)]
tokio::task_local! {
    static RENEWAL_RECHECK_TIME: DateTime<Utc>;
}

#[cfg(test)]
pub(crate) async fn renew_with_recheck_at(
    node: &EmbeddedNode,
    request_doc_id: &str,
    generation: &str,
    expected_deadline: DateTime<Utc>,
    admission: DateTime<Utc>,
    recheck: DateTime<Utc>,
) -> Result<RenewalAttemptOutcome> {
    RENEWAL_RECHECK_TIME
        .scope(
            recheck,
            renew_once_at(
                node,
                request_doc_id,
                generation,
                expected_deadline,
                admission,
            ),
        )
        .await
}

fn renewal_recheck_time(fixture_now: Option<DateTime<Utc>>) -> DateTime<Utc> {
    #[cfg(test)]
    if let Ok(now) = RENEWAL_RECHECK_TIME.try_with(|now| *now) {
        return now;
    }
    fixture_now.unwrap_or_else(Utc::now)
}

async fn renew_once_with_time(
    node: &EmbeddedNode,
    request_doc_id: &str,
    generation: &str,
    expected_deadline: Option<DateTime<Utc>>,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<RenewalAttemptOutcome> {
    match crate::config_client::ConfigAccess::renew_execution_lease(
        node,
        request_doc_id,
        generation,
        expected_deadline,
        fixture_now,
    )
    .await
    {
        Err(error) if error.is::<RenewalAdmissionExpired>() => Ok(RenewalAttemptOutcome::Lost),
        result => result,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("execution lease expired before renewal commit admission")]
struct RenewalAdmissionExpired;

pub(crate) async fn renew_in_transaction(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    generation: &str,
    expected_deadline: Option<DateTime<Utc>>,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<RenewalAttemptOutcome> {
    let started = std::time::Instant::now();
    let doc_id = escape_graphql_string(request_doc_id);
    let result = txn.execute_local_response(&format!(r#"{{ AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1
            ) {{ request_id lifecycle_state execution_generation execution_lease_expires_at execution_lease_secs }} }}"#)).await?;
    let snapshot_read_ms = started.elapsed().as_millis() as u64;
    let Some(row) = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")? else {
        return Ok(RenewalAttemptOutcome::Lost);
    };
    let state = row.lifecycle_state.context("missing execution lifecycle")?;
    if !renewable_lifecycle(state) || row.execution_generation.as_deref() != Some(generation) {
        return Ok(RenewalAttemptOutcome::Lost);
    }
    let owner = row
        .execution_generation
        .as_deref()
        .context("missing execution generation")?;
    let expiry = row
        .execution_lease_expires_at
        .as_deref()
        .context("missing execution deadline")?;
    let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
    let duration = row
        .execution_lease_secs
        .context("missing execution duration")?
        .checked_mul(1000)
        .context("execution duration overflow")?;
    anyhow::ensure!(duration > 0, "execution duration must be positive");
    let now = fixture_now.unwrap_or_else(Utc::now).timestamp_millis();
    let observed = LeaseObservation {
        request: state,
        generation: owner,
        deadline_ms: deadline,
    };
    if !renewable_lifecycle(state) || !is_live(observed, generation, now) {
        tracing::warn!(
            %request_doc_id, %generation, snapshot_read_ms,
            expired_by_ms = now.saturating_sub(deadline),
            lease_duration_ms = duration,
            "execution lease renewal observed an expired lease"
        );
        return Ok(RenewalAttemptOutcome::Lost);
    }
    let expected = expected_deadline
        .as_ref()
        .map_or(deadline, DateTime::timestamp_millis);
    if expected != deadline {
        return Ok(RenewalAttemptOutcome::Lost);
    }
    let Some(next) = authorize_renewal(observed, generation, expected, duration, now) else {
        return Ok(RenewalAttemptOutcome::Skipped);
    };
    let next = DateTime::<Utc>::from_timestamp_millis(next).context("renewal date overflow")?;
    let owner = escape_graphql_string(owner);
    let state = escape_graphql_string(state.as_str());
    let expiry = escape_graphql_string(expiry);
    let next = escape_graphql_string(&next.to_rfc3339());
    let result = txn.execute_local_response(&format!(r#"mutation {{ update_AgentRequest(
                docID: "{doc_id}", filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{owner}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
                input: {{ execution_lease_expires_at: "{next}" }}
            ) {{ _docID }} }}"#)).await?;
    anyhow::ensure!(
        result
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents),
        "lease renewal lost deadline CAS"
    );
    let recheck = renewal_recheck_time(fixture_now).timestamp_millis();
    if !is_live(observed, generation, recheck) {
        tracing::warn!(
            %request_doc_id, %generation, snapshot_read_ms,
            expired_by_ms = recheck.saturating_sub(deadline),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "execution lease expired during renewal commit admission"
        );
        return Err(RenewalAdmissionExpired.into());
    }
    Ok(RenewalAttemptOutcome::Committed)
}
