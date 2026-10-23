use std::sync::Arc;

use anyhow::{ensure, Result};
use defra_node::EmbeddedNode;
use gents_protocol::file_inventory::{FileInventoryPageDemand, FileInventoryReply};

use crate::NodeIdentity;

use super::{delivery, FileInventorySnapshot};

#[derive(Debug, thiserror::Error)]
#[error("file inventory requires the authenticated runtime operator")]
pub struct OperatorInventoryDenied;

fn require_operator(
    node: &EmbeddedNode,
    identity: &dyn NodeIdentity,
    authenticated_actor: &str,
    demand: &FileInventoryPageDemand,
) -> Result<()> {
    let allowed = node.node_identity_did().is_some_and(|owner| {
        !owner.is_empty()
            && authenticated_actor == owner
            && identity.did() == owner
            && demand.admission.request.node_did == owner
    });
    ensure!(allowed, OperatorInventoryDenied);
    Ok(())
}

/// The caller obtains the actor from DefraDB's authenticated request identity.
/// DefraDB's native node DID always owns its NAC policy. This read reuses that
/// identity and the existing canonical/ACP loader; it creates no enrollment or
/// image grant. Preparation preserves a page across HTTP reply loss, without
/// asserting an explicit-transfer acknowledgment or settling a control request.
pub async fn read_operator_inventory(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    authenticated_actor: &str,
    demand: &FileInventoryPageDemand,
) -> Result<FileInventoryReply> {
    require_operator(node, identity, authenticated_actor, demand)?;
    demand.validate_reference()?;
    if let Some(prior) = super::load_recorded_reply(node, demand, identity).await? {
        require_operator(node, identity, authenticated_actor, demand)?;
        return Ok(prior);
    }
    let prepared = match delivery::load_prepared_page(node, demand, identity).await? {
        Some(prepared) => prepared,
        None => {
            let selected = async {
                let snapshot = FileInventorySnapshot::load(node, &demand.admission).await?;
                snapshot
                    .page(demand.cursor.as_ref())
                    .map_err(anyhow::Error::from)
            }
            .await;
            let page = match selected {
                Ok(page) => page,
                Err(error) => {
                    let reason = error.downcast_ref::<super::FileInventoryPageError>()
                        .map(super::FileInventoryPageError::refusal_reason)
                        .unwrap_or_else(|| crate::agent::p2p_reconcile::session_hydration_reconcile::file_inventory_refusal_reason(&error));
                    let receipt = super::sign_refusal(identity, demand, reason).await?;
                    require_operator(node, identity, authenticated_actor, demand)?;
                    return super::record_reply(
                        node,
                        demand,
                        &FileInventoryReply::Refused(receipt),
                        identity,
                    )
                    .await;
                }
            };
            let receipt =
                super::sign_current_page(identity, &demand.derive_request_key()?, page, || {
                    std::future::ready(
                        require_operator(node, identity, authenticated_actor, demand)
                            .map(|()| true),
                    )
                })
                .await?;
            delivery::prepare_page(node, demand, &receipt, identity).await?
        }
    };
    require_operator(node, identity, authenticated_actor, demand)?;
    Ok(FileInventoryReply::Page(prepared.receipt))
}
