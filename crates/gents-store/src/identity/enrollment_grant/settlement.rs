use gents_protocol::owed_settlement::OwedStanding;

use super::{
    close_owed_in_txn, decode_receipts, load_enrollment_grant_receipts, EnrollmentGrantError,
    EnrollmentGrantTuple, NODE_ADMINISTRATOR_RELATION,
};
use crate::config_client::{ConfigAccess, ConfigClientError};

/// Lean `Identity.EnrollmentGrant.SettlementStep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementStep {
    Issue,
    Close,
    Leave,
}

/// Lean `Identity.EnrollmentGrant.settlementStep`.
pub fn settlement_step(owed: bool, taken_over: bool) -> SettlementStep {
    match (owed, taken_over) {
        (true, true) => SettlementStep::Close,
        (true, false) => SettlementStep::Issue,
        (false, _) => SettlementStep::Leave,
    }
}

/// Lean `Identity.EnrollmentGrant.AfterRemoval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterRemoval {
    Reissue,
    Close,
}

/// Lean `Identity.EnrollmentGrant.afterRemoval`.
pub fn after_removal(taken_over: bool) -> AfterRemoval {
    match taken_over {
        true => AfterRemoval::Reissue,
        false => AfterRemoval::Close,
    }
}

/// One member's receipts as the settlement decides from them.
struct Owing {
    member: identity::Did,
    owed: Vec<String>,
    restoring: Vec<String>,
    taken_over: bool,
}

async fn owing(runtime: &ConfigAccess, member: &str) -> Result<Owing, EnrollmentGrantError> {
    let receipts = load_enrollment_grant_receipts(runtime, member).await?;
    Ok(Owing {
        member: identity::Did::new(member).map_err(EnrollmentGrantError::MemberIdentity)?,
        owed: receipts
            .iter()
            .filter(|receipt| receipt.owed)
            .map(|receipt| receipt.doc_id.clone())
            .collect(),
        restoring: receipts
            .iter()
            .filter(|receipt| receipt.restore)
            .map(|receipt| receipt.doc_id.clone())
            .collect(),
        taken_over: receipts
            .iter()
            .any(|receipt| receipt.accounts(member, EnrollmentGrantTuple::NodeAdministrator)),
    })
}

/// Every member some receipt still owes an administration removal for, and,
/// with `restorations`, every member owed the relation's restoration, as the
/// runtime reads them.
async fn owed_members(
    runtime: &ConfigAccess,
    restorations: bool,
) -> Result<Vec<String>, EnrollmentGrantError> {
    use gents_protocol::document_query::{
        fields::network_enrollment_grant as fields, Filter, Query, Selection,
    };
    let rendered = Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::MEMBER_DID)
            .field(fields::TUPLES)
            .field(fields::WITHDRAWN_AT)
            .field(fields::WITHDRAWAL_OWED_AT)
            .field(fields::RESTORATION_OWED_AT),
    )
    .filter(match restorations {
        true => Filter::any([
            Filter::not_null(fields::WITHDRAWAL_OWED_AT),
            Filter::not_null(fields::RESTORATION_OWED_AT),
        ]),
        false => Filter::not_null(fields::WITHDRAWAL_OWED_AT),
    })
    .render();
    let response = runtime
        .read_for_decision(&rendered)
        .await
        .map_err(EnrollmentGrantError::Read)?;
    let mut members: Vec<String> = decode_receipts(&response)?
        .into_iter()
        .map(|receipt| receipt.member_did)
        .collect();
    members.sort();
    members.dedup();
    Ok(members)
}

async fn close(runtime: &ConfigAccess, owed: &[String]) -> Result<(), EnrollmentGrantError> {
    if owed.is_empty() {
        return Ok(());
    }
    runtime
        .transact_write("enrollment_grant.close_owed", |txn| {
            Box::pin(async move {
                let owed: Vec<&str> = owed.iter().map(String::as_str).collect();
                close_owed_in_txn(txn, &owed).await
            })
        })
        .await
}

enum Restoration {
    Closed,
    NotTheCallers,
}

/// Lean `restoreBy`: observe the relation as `caller` and restore it unless
/// it is held or its removal is owed again (`restorationStep`). The
/// restoration closes only after that.
async fn restore_as(
    caller: &ConfigAccess,
    node_doc_id: &str,
    owing: &Owing,
) -> Result<Restoration, crate::config_client::ConfigClientError> {
    let held = match caller
        .observe_actor_relationship(
            "Node",
            node_doc_id,
            NODE_ADMINISTRATOR_RELATION,
            &owing.member,
        )
        .await
    {
        Err(refused) if refused.is_permission_denied() => return Ok(Restoration::NotTheCallers),
        Err(error) => return Err(error),
        Ok(held) => held,
    };
    if restoration_step(!owing.owed.is_empty(), held) == RestorationStep::Restore {
        caller
            .add_actor_relationship(
                "Node",
                node_doc_id,
                NODE_ADMINISTRATOR_RELATION,
                &owing.member,
            )
            .await?;
    }
    Ok(Restoration::Closed)
}

async fn clear_restoration(
    runtime: &ConfigAccess,
    restoring: &[String],
) -> Result<(), EnrollmentGrantError> {
    use gents_protocol::document_query::{fields::network_enrollment_grant as fields, Input};
    runtime
        .transact_write("enrollment_grant.close_restoration", |txn| {
            Box::pin(async move {
                for receipt in restoring {
                    super::update_receipt_in_txn(
                        txn,
                        receipt,
                        Input::new().set_null(fields::RESTORATION_OWED_AT),
                    )
                    .await?;
                }
                Ok(())
            })
        })
        .await
}

/// Lean `Identity.EnrollmentGrant.RestorationStep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorationStep {
    Restore,
    Close,
}

/// Lean `Identity.EnrollmentGrant.restorationStep`.
pub fn restoration_step(owed: bool, held: bool) -> RestorationStep {
    match owed || held {
        true => RestorationStep::Close,
        false => RestorationStep::Restore,
    }
}

enum Removal {
    Removed,
    NotTheCallers,
}

/// Remove `member`'s Node `administrator` relation as `caller`. Only an actor
/// that may manage the relation may observe it (Lean
/// `observeAdministrator`), so a caller refused the observation is refused
/// the removal too, and the withdrawal is not the caller's to settle; an
/// absent relation needs no removal.
async fn remove_as(
    caller: &ConfigAccess,
    node_doc_id: &str,
    member: &identity::Did,
) -> Result<Removal, crate::config_client::ConfigClientError> {
    match caller
        .observe_actor_relationship("Node", node_doc_id, NODE_ADMINISTRATOR_RELATION, member)
        .await
    {
        Err(refused) if refused.is_permission_denied() => Ok(Removal::NotTheCallers),
        Err(error) => Err(error),
        Ok(false) => Ok(Removal::Removed),
        Ok(true) => {
            caller
                .delete_actor_relationship("Node", node_doc_id, NODE_ADMINISTRATOR_RELATION, member)
                .await?;
            Ok(Removal::Removed)
        }
    }
}

/// Issue, as `caller` and under the credential its access carries, every
/// administration withdrawal the home owes (Lean
/// `Identity.EnrollmentGrant.settle`, `settlementStep`, `afterRemoval`), and
/// report the standing of each the caller may issue. A withdrawal the caller
/// may not issue stays owed and is not reported: it is not the caller's to
/// settle. `runtime` reads and closes the receipts it owns.
pub async fn settle_owed_withdrawals(
    caller: &ConfigAccess,
    runtime: &ConfigAccess,
    node_doc_id: &str,
) -> Result<Vec<OwedStanding>, EnrollmentGrantError> {
    let mut standings = Vec::new();
    for member in owed_members(runtime, true).await? {
        let before = owing(runtime, &member).await?;
        if !before.restoring.is_empty() {
            match restore_as(caller, node_doc_id, &before).await {
                Ok(Restoration::Closed) => {
                    clear_restoration(runtime, &before.restoring).await?;
                    standings.push(OwedStanding::Settled);
                }
                Ok(Restoration::NotTheCallers) => continue,
                Err(error) => {
                    tracing::warn!(%error, member, "an owed administration restoration was not issued");
                    standings.push(OwedStanding::Sending);
                }
            }
        }
        match settlement_step(!before.owed.is_empty(), before.taken_over) {
            SettlementStep::Leave => {}
            SettlementStep::Close => close(runtime, &before.owed).await?,
            SettlementStep::Issue => {
                match remove_as(caller, node_doc_id, &before.member).await {
                    Ok(Removal::Removed) => {}
                    Ok(Removal::NotTheCallers) => continue,
                    Err(error) => {
                        tracing::warn!(%error, member, "an owed administration withdrawal was not issued");
                        standings.push(OwedStanding::Sending);
                        continue;
                    }
                }
                close(runtime, &before.owed).await?;
                let after = owing(runtime, &member).await?;
                let reissued = match after_removal(after.taken_over) {
                    AfterRemoval::Close => Ok(()),
                    AfterRemoval::Reissue => {
                        caller
                            .add_actor_relationship(
                                "Node",
                                node_doc_id,
                                NODE_ADMINISTRATOR_RELATION,
                                &after.member,
                            )
                            .await
                    }
                };
                match reissued {
                    Ok(()) => standings.push(OwedStanding::Settled),
                    Err(error) => {
                        tracing::warn!(%error, member, "the relation an approval took over stays owed its restoration");
                        standings.push(OwedStanding::Sending);
                    }
                }
            }
        }
    }
    Ok(standings)
}

/// Lean `permissionGrantAdmitted`.
pub fn permission_grant_admitted(owed: bool) -> bool {
    !owed
}

/// The runtime principal's access to `node`, when `node` serves a runtime
/// that keeps enrollment grant receipts.
fn receipt_reader(node: &std::sync::Arc<defra_node::Node>) -> Option<ConfigAccess> {
    let runtime = identity::Did::new(node.node_identity_did()?).ok()?;
    node.get_collection(super::RECEIPT_COLLECTION)
        .ok()
        .flatten()?;
    Some(ConfigAccess::local_as(node.clone(), runtime))
}

/// The receipts on `node` that owe the removal of `member`'s Node
/// `administrator` relation (Lean `owedFor`), as the runtime reads them.
pub(crate) async fn owed_removal_receipts(
    node: &std::sync::Arc<defra_node::Node>,
    member: &str,
) -> Result<Vec<String>, ConfigClientError> {
    removal_receipts(node, member, None).await
}

async fn removal_receipts(
    node: &std::sync::Arc<defra_node::Node>,
    member: &str,
    document: Option<&str>,
) -> Result<Vec<String>, ConfigClientError> {
    use gents_protocol::document_query::{
        fields::network_enrollment_grant as fields, Filter, Query, Selection,
    };
    let Some(runtime) = receipt_reader(node) else {
        return Ok(Vec::new());
    };
    let mut filter =
        Filter::eq(fields::MEMBER_DID, member).and(Filter::not_null(fields::WITHDRAWAL_OWED_AT));
    if let Some(doc) = document {
        filter = filter.and(Filter::eq(fields::NODE_DOC_ID, doc));
    }
    let rendered = Query::select(Selection::doc_id()).filter(filter).render();
    let response = runtime.read_for_decision(&rendered).await?;
    Ok(
        gents_protocol::graphql::graphql_rows_from_response(&response, super::RECEIPT_COLLECTION)
            .iter()
            .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
            .collect(),
    )
}

/// Take `member`'s owed removal on `node` over after a deliberate grant of
/// the relation (Lean `grantOther`, `takeOver`).
pub(crate) async fn close_owed_removal(
    node: &std::sync::Arc<defra_node::Node>,
    doc: &str,
    member: &str,
) -> Result<(), ConfigClientError> {
    let owed = removal_receipts(node, member, Some(doc)).await?;
    let Some(runtime) = receipt_reader(node) else {
        return Ok(());
    };
    if owed.is_empty() {
        return Ok(());
    }
    runtime
        .transact_write("enrollment_grant.take_over_owed", |txn| {
            let owed = &owed;
            Box::pin(async move {
                let owed: Vec<&str> = owed.iter().map(String::as_str).collect();
                super::take_over_in_txn(txn, &owed).await
            })
        })
        .await
        .map_err(|error| match error {
            EnrollmentGrantError::Retire(error) => error,
            _ => ConfigClientError::OwedRemovalUnconfirmed,
        })
}

/// Lean `lapseClock`: the provider's time where there is one, bounded below by
/// the node's wall clock, in Unix seconds.
pub fn lapse_clock(provider: Option<u64>, wall: u64) -> u64 {
    provider.unwrap_or(0).max(wall)
}

/// Lean `membershipLapsed`: every current authorization lease of the member,
/// in Unix seconds, has ended on the lapse clock.
pub fn membership_lapsed(provider: Option<u64>, wall: u64, leases: &[u64]) -> bool {
    let now = lapse_clock(provider, wall);
    !leases.is_empty() && leases.iter().all(|expires| *expires <= now)
}

/// The administration removals the home owes that `caller` may complete:
/// those whose relation it may observe, which only a manager of the relation
/// may (Lean `observeAdministrator`).
pub async fn owed_removals(
    caller: &ConfigAccess,
    runtime: &ConfigAccess,
    node_doc_id: &str,
) -> Result<Vec<gents_protocol::owed_settlement::OwedRemoval>, EnrollmentGrantError> {
    let mut removals = Vec::new();
    for member in owed_members(runtime, false).await? {
        let did = identity::Did::new(&member).map_err(EnrollmentGrantError::MemberIdentity)?;
        match caller
            .observe_actor_relationship("Node", node_doc_id, NODE_ADMINISTRATOR_RELATION, &did)
            .await
        {
            Ok(administrator_held) => removals.push(gents_protocol::owed_settlement::OwedRemoval {
                member_did: member,
                administrator_held,
            }),
            Err(refused) if refused.is_permission_denied() => {}
            Err(error) => return Err(EnrollmentGrantError::Observe(error)),
        }
    }
    Ok(removals)
}
