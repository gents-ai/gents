use anyhow::Context;
use defra_node::Node as EmbeddedNode;
use gents::store::config_client::ConfigAccess;
use std::sync::Arc;

use super::correlated::{Decision, Outcome, Record};
use super::{
    journal, resolve_unresolved_submission, SubmissionAttemptError, SubmissionError,
    SubmissionNotDelivered, SubmittedRequest,
};

/// The owner access a reconcile may replay through. `NotRequested` is a record
/// the caller judged settled; reaching replay with it keeps the submission
/// unknown without a cause, so no fence refusal is invented.
pub(crate) enum OwnerAccess<'a> {
    Granted(&'a ConfigAccess),
    Refused(anyhow::Error),
    NotRequested,
}

/// A retained preparation is the only replay source. Confirmed outcomes are
/// persisted before returning them through the outer caller transport.
pub(crate) async fn reconcile(
    node: &Arc<EmbeddedNode>,
    access: OwnerAccess<'_>,
    record: Record,
) -> std::result::Result<SubmittedRequest, SubmissionError> {
    match record.result() {
        Decision::Settled(reference, Outcome::Accepted) => return Ok(reference.request),
        Decision::Settled(reference, Outcome::NotDelivered) => {
            return Err(SubmissionNotDelivered::Rejected {
                request: reference.request,
                cause: None,
            }
            .into());
        }
        Decision::Reconcile(_) => {}
        _ => return Err(anyhow::anyhow!("caller submission receipt binding was refused").into()),
    }
    let pending = record
        .pending
        .as_ref()
        .context("unresolved caller receipt omitted its signed preparation")?;
    let access = match access {
        OwnerAccess::Granted(access) => access,
        OwnerAccess::Refused(cause) => return Err(pending.unknown(Some(cause)).into()),
        OwnerAccess::NotRequested => return Err(pending.unknown(None).into()),
    };
    let delivered = resolve_unresolved_submission(access, pending).await;
    let (outcome, rejection) = match delivered {
        Ok(_) => (Outcome::Accepted, None),
        Err(SubmissionAttemptError::Unknown(error)) => return Err(error.into()),
        Err(SubmissionAttemptError::Rejected(error)) => (Outcome::NotDelivered, Some(error.into())),
    };
    let settled =
        match journal::caller::update(node, &record.intent, &record.reference, Some(outcome)).await
        {
            Ok(settled) => settled,
            // A failed local outcome write cannot authorize a fresh preparation.
            Err(error) => return Err(pending.unknown(Some(error)).into()),
        };
    match settled.result() {
        Decision::Settled(reference, Outcome::Accepted) => Ok(reference.request),
        Decision::Settled(reference, Outcome::NotDelivered) => {
            Err(SubmissionNotDelivered::Rejected {
                request: reference.request,
                cause: rejection,
            }
            .into())
        }
        _ => Err(pending.unknown(None).into()),
    }
}
