use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::store::config_client::ConfigAccess;
use std::sync::Arc;

use super::correlated::{Decision, Outcome, Record};
use super::{
    journal, resolve_unresolved_submission, SubmissionNotDelivered, SubmissionOutcomeUnknown,
    SubmittedRequest,
};

/// A retained preparation is the only replay source. Confirmed outcomes are
/// persisted before returning them through the outer caller transport.
pub(crate) async fn reconcile(
    node: &Arc<EmbeddedNode>,
    access: Option<&ConfigAccess>,
    record: Record,
) -> Result<SubmittedRequest> {
    match record.result() {
        Decision::Settled(reference, Outcome::Accepted) => return Ok(reference.request),
        Decision::Settled(reference, Outcome::NotDelivered) => {
            return Err(SubmissionNotDelivered {
                request: reference.request,
            }
            .into());
        }
        Decision::Reconcile(_) => {}
        _ => anyhow::bail!("caller submission receipt binding was refused"),
    }
    let pending = record
        .pending
        .as_ref()
        .context("unresolved caller receipt omitted its signed preparation")?;
    let Some(access) = access else {
        return Err(pending.outcome_unknown(None));
    };
    let delivered = resolve_unresolved_submission(access, pending).await;
    let outcome = match &delivered {
        Ok(_) => Outcome::Accepted,
        Err(error) if error.is::<SubmissionOutcomeUnknown>() => return delivered,
        Err(_) => Outcome::NotDelivered,
    };
    let settled =
        match journal::caller::update(node, &record.intent, &record.reference, Some(outcome)).await
        {
            Ok(settled) => settled,
            // A failed local outcome write cannot authorize a fresh preparation.
            Err(error) => return Err(pending.outcome_unknown(Some(error))),
        };
    match settled.result() {
        Decision::Settled(reference, Outcome::Accepted) => Ok(reference.request),
        Decision::Settled(reference, Outcome::NotDelivered) => Err(SubmissionNotDelivered {
            request: reference.request,
        }
        .into()),
        _ => Err(pending.outcome_unknown(None)),
    }
}
