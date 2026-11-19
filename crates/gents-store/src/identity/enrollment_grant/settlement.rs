use gents_protocol::owed_settlement::OwedStanding;

use super::{
    close_owed_in_txn, decode_receipts, load_enrollment_grant_receipts, EnrollmentGrantError,
    EnrollmentGrantTuple, NODE_ADMINISTRATOR_RELATION,
};
use crate::config_client::ConfigAccess;

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
        taken_over: receipts
            .iter()
            .any(|receipt| receipt.accounts(member, EnrollmentGrantTuple::NodeAdministrator)),
    })
}

/// Every member some retired receipt still owes an administration withdrawal
/// for, as the runtime reads them.
async fn owed_members(runtime: &ConfigAccess) -> Result<Vec<String>, EnrollmentGrantError> {
    use gents_protocol::document_query::{
        fields::network_enrollment_grant as fields, Filter, Query, Selection,
    };
    let rendered = Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::MEMBER_DID)
            .field(fields::TUPLES)
            .field(fields::WITHDRAWN_AT)
            .field(fields::WITHDRAWAL_OWED_AT),
    )
    .filter(Filter::not_null(fields::WITHDRAWAL_OWED_AT))
    .render();
    let response = runtime
        .read(&rendered)
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
    for member in owed_members(runtime).await? {
        let before = owing(runtime, &member).await?;
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
                let after = owing(runtime, &member).await?;
                if after_removal(after.taken_over) == AfterRemoval::Reissue {
                    caller
                        .add_actor_relationship(
                            "Node",
                            node_doc_id,
                            NODE_ADMINISTRATOR_RELATION,
                            &after.member,
                        )
                        .await
                        .map_err(|source| EnrollmentGrantError::Issue {
                            tuple: EnrollmentGrantTuple::NodeAdministrator.name(),
                            source,
                        })?;
                }
                close(runtime, &after.owed).await?;
                standings.push(OwedStanding::Settled);
            }
        }
    }
    Ok(standings)
}
