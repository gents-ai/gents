use super::{handling, serves_pass, standing, Handling, OwedStanding, Recorded};
use crate::config_client::user_write::{self, PresentedDelegation};
use crate::node_scope::ContextCalls as _;

#[derive(Debug, thiserror::Error)]
pub enum OwedPassError {
    #[error(
        "settling what a User's writes owe takes that User's own delegation as the request's \
         bearer, and the request presented none"
    )]
    DelegationRequired,
    #[error("reading what the caller owes the document access control")]
    Report(#[source] defra_node::Error),
}

fn standing_of(entry: &acp::OwedEntry) -> OwedStanding {
    let recorded = match entry.state {
        acp::OwedState::Staged => Recorded::Staged,
        acp::OwedState::AwaitingAuthority => Recorded::AwaitingAuthority,
        acp::OwedState::Refused { .. } => Recorded::Refused,
    };
    standing(
        recorded,
        entry.sendable,
        entry.held_by.is_some(),
        entry.discarded,
    )
}

/// One settlement pass for the caller `context` authenticates: the
/// delegation its request presented is handed to DefraDB for each of its own
/// entries that waits for authority (`Identity.OwedRegistration.handling`),
/// and the standing of each entry is reported. The runtime signs nothing.
pub async fn settle_pass(
    node: &defra_node::Node,
    context: &defra_core::RequestContext,
    caller: &str,
) -> Result<Vec<OwedStanding>, OwedPassError> {
    let presented = PresentedDelegation::of_request(node, context, caller);
    let verdict = presented
        .as_ref()
        .map_or(user_write::Verdict::Refused, |presented| {
            user_write::verdict(node, caller, &presented.token())
        });
    let presented = match presented {
        Some(presented) if serves_pass(true, verdict) => presented,
        _ => return Err(OwedPassError::DelegationRequired),
    };
    let entries = node
        .owed_acp_as(context, caller)
        .await
        .map_err(OwedPassError::Report)?;
    let mut standings = Vec::with_capacity(entries.len());
    for entry in entries {
        let observed = standing_of(&entry);
        let standing = match handling(observed) {
            Handling::Leave => observed,
            Handling::HandIn => {
                match node
                    .reauthorize_owed_acp_as(
                        context,
                        &entry.reference(),
                        defra_node::SignerCredential::Delegation(presented.token()),
                    )
                    .await
                {
                    Ok(acp::OwedOutcome::Settled) => OwedStanding::Settled,
                    Ok(acp::OwedOutcome::Owed(entry)) => standing_of(&entry),
                    Ok(acp::OwedOutcome::Discarded(_)) => OwedStanding::Failed,
                    Err(defra_node::Error::Acp(acp::Error::OwedEntryNotFound { .. })) => {
                        OwedStanding::Settled
                    }
                    Err(error) => {
                        tracing::warn!(%error, entry = %entry.reference(), "an owed entry was not handed a fresh delegation");
                        observed
                    }
                }
            }
        };
        standings.push(standing);
    }
    Ok(standings)
}
