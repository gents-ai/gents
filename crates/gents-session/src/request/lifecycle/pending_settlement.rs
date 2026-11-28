use anyhow::{Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::TtlOutcome;

/// Lean `RequestContext.PendingDisposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingDisposition {
    Expired,
    Open,
}

/// Lean `RequestContext.pendingDisposition`.
pub(crate) fn pending_disposition(ttl: &TtlOutcome) -> PendingDisposition {
    match ttl {
        TtlOutcome::Expired(_) => PendingDisposition::Expired,
        TtlOutcome::NotSet | TtlOutcome::Live(_) | TtlOutcome::Malformed(_) => {
            PendingDisposition::Open
        }
    }
}

/// Reads the pending row by `_docID` inside the refusal's own transaction
/// and, when its TTL has passed at `terminalized_at`, writes the claim's
/// dead (Stale) terminal (Lean `pendingDisposition`). `None` when no pending
/// row matches; `Some(Open)` writes nothing and leaves the row to the
/// refusal's own terminal.
pub(crate) async fn settle_pending_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    node_did: &str,
    agent_did: Option<&str>,
    terminalized_at: &str,
) -> Result<Option<PendingDisposition>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let mut read = Query::select(Selection::new().field(fields::VALID_UNTIL))
        .filter(Filter::eq(fields::DOC_ID, doc_id))
        .filter(Filter::eq(fields::NODE_DID, node_did))
        .filter(Filter::eq(
            fields::LIFECYCLE_STATE,
            RequestLifecycleState::Pending.as_str(),
        ));
    if let Some(agent_did) = agent_did {
        read = read.filter(Filter::eq(fields::AGENT_DID, agent_did));
    }
    let response = txn.run(&read.limit(2).render()).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("pending settlement read omitted request rows")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "pending settlement matched multiple requests"
    );
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let now = chrono::DateTime::parse_from_rfc3339(terminalized_at)?.with_timezone(&chrono::Utc);
    let valid_until = row["valid_until"]
        .as_str()
        .filter(|value| !value.is_empty());
    match pending_disposition(&super::parse_valid_until(valid_until, now)) {
        PendingDisposition::Open => Ok(Some(PendingDisposition::Open)),
        PendingDisposition::Expired => {
            Ok(
                expire_pending_in_txn(txn, doc_id, node_did, terminalized_at)
                    .await?
                    .then_some(PendingDisposition::Expired),
            )
        }
    }
}

/// Moves a pending request to dead (Stale) by compare-and-set on
/// `lifecycle_state`, releases its writer binding and stages its terminal
/// hook when the row names a request. Returns whether the row was pending.
pub(crate) async fn expire_pending_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    node_did: &str,
    terminalized_at: &str,
) -> Result<bool> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Input, Selection, Update,
    };
    let transition = RequestLifecycleState::Pending.transition_to(RequestLifecycleState::Dead)?;
    let mutation = Update::doc(
        doc_id,
        Input::new()
            .set(fields::LIFECYCLE_STATE, transition.to().as_str())
            .set(fields::FAILURE_REASON, "Stale")
            .set(fields::TERMINALIZED_AT, terminalized_at)
            .set(fields::TERMINAL_REDRIVE_ATTEMPTS, 0_i32),
    )
    .filter(Filter::eq(fields::DOC_ID, doc_id))
    .filter(Filter::eq(fields::NODE_DID, node_did))
    .filter(Filter::eq(
        fields::LIFECYCLE_STATE,
        transition.from().as_str(),
    ))
    .returning(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::WORKSPACE_ID)
            .field(fields::WORKSPACE_OWNER_NODE_DID),
    )
    .render();
    let response = txn.run_local_response(&mutation).await?;
    let Some(rows) = response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .filter(|rows| !rows.is_empty())
    else {
        return Ok(false);
    };
    for row in rows {
        crate::request::workspace_binding::release_terminal_writer_binding(txn, row).await?;
        if let Some(request_id) = row["request_id"].as_str() {
            crate::request::hooks::points::request_terminal(
                txn,
                node_did,
                request_id,
                transition.to(),
                "Stale",
                terminalized_at,
            )
            .await?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod conformance {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum TtlObservation {
        NotSet,
        Live,
        Expired,
        Malformed,
    }

    fn valid_until(ttl: &TtlObservation) -> Option<&'static str> {
        match ttl {
            TtlObservation::NotSet => None,
            TtlObservation::Live => Some("2026-09-01T00:00:01Z"),
            TtlObservation::Expired => Some("2026-08-31T23:59:59Z"),
            TtlObservation::Malformed => Some("not-a-time"),
        }
    }

    #[test]
    fn pending_disposition_matches_lean() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        gents_lean_vocab::families::request_context_pending_disposition::replay(
            |i: &gents_lean_vocab::families::request_context_pending_disposition::Input<
                TtlObservation,
            >| {
                super::pending_disposition(&super::super::parse_valid_until(
                    valid_until(&i.ttl),
                    now,
                ))
            },
        );
    }
}
