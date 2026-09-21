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
pub(super) struct RenewalTask(tokio::task::JoinHandle<()>);

impl Drop for RenewalTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl RenewalTask {
    pub(super) fn start(
        node: Arc<EmbeddedNode>,
        request_doc_id: String,
        generation: String,
        duration_ms: u64,
    ) -> Self {
        Self(tokio::spawn(async move {
            // Poll more often than renewal is due, but only the bounded policy
            // writes. Skip missed ticks after suspension; never catch up with
            // a burst of renewals or revive an expired generation.
            let mut ticker = tokio::time::interval(Duration::from_millis((duration_ms / 4).max(1)));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match renew_once(&node, &request_doc_id, &generation).await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        // Do not invent liveness on errors. The next bounded
                        // poll rereads authoritative state; expiry remains final.
                        tracing::warn!(%error, %request_doc_id, "execution lease renewal failed");
                    }
                }
            }
        }))
    }
}

/// Return false when this generation is no longer an active owner. A true
/// result includes an early poll that performed no mutation.
async fn renew_once(node: &EmbeddedNode, request_doc_id: &str, generation: &str) -> Result<bool> {
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node, None, crate::config_client::IdempotentTransactionRetry::Standard,
        "lifecycle.renew_execution_lease", move |txn| Box::pin(async move {
            let doc_id = escape_graphql_string(request_doc_id);
            let result = txn.execute_local_response(&format!(r#"{{ AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1
            ) {{ request_id lifecycle_state execution_generation execution_lease_expires_at execution_lease_secs }} }}"#)).await?;
            let Some(row) = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")? else {
                return Ok(false);
            };
            let state = row.lifecycle_state.context("missing execution lifecycle")?;
            if !renewable_lifecycle(state) || row.execution_generation.as_deref() != Some(generation) {
                return Ok(false);
            }
            let owner = row.execution_generation.as_deref().context("missing execution generation")?;
            let expiry = row.execution_lease_expires_at.as_deref().context("missing execution deadline")?;
            let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
            let duration = row.execution_lease_secs.context("missing execution duration")?
                .checked_mul(1000).context("execution duration overflow")?;
            anyhow::ensure!(duration > 0, "execution duration must be positive");
            let now = Utc::now().timestamp_millis();
            let observed = LeaseObservation { request: state, generation: owner, deadline_ms: deadline };
            if !renewable_lifecycle(state) || !is_live(observed, generation, now) {
                return Ok(false);
            }
            let Some(next) = authorize_renewal(observed, generation, deadline, duration, now) else {
                return Ok(true);
            };
            let next = DateTime::<Utc>::from_timestamp_millis(next).context("renewal date overflow")?;
            let owner = escape_graphql_string(owner);
            let state = escape_graphql_string(state.as_str());
            let expiry = escape_graphql_string(expiry);
            let next = escape_graphql_string(&next.to_rfc3339());
            let result = txn.execute_local_response(&format!(r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{owner}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
                input: {{ execution_lease_expires_at: "{next}" }}
            ) {{ _docID }} }}"#)).await?;
            anyhow::ensure!(result.data.as_ref().and_then(|data| data.get("update_AgentRequest"))
                .is_some_and(response_has_documents), "lease renewal lost deadline CAS");
            Ok(true)
        }),
    ).await
}
