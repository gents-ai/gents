//! Lean `DocumentOperations.ownerResolution` for a document a caller names by
//! its ID alone: every read and write that resolves one ends here.

use gents_protocol::document_query::{fields, rows, Filter, Query, Rendered, Selection};
use gents_protocol::operations::document::{DocumentOperationError, OwnerCount, OwnerResolution};
use gents_store::config_client::ConfigAccess;

use super::decide_owner;

/// The owner a call acts on: `owner` when the caller named one, otherwise the
/// only node holding `id` among the documents the acting identity can read.
/// Several owners are refused, naming them.
async fn resolve_owner(
    access: &ConfigAccess,
    collection: &'static str,
    owner: Option<&str>,
    id: &str,
    owners_of: Rendered,
) -> Result<String, DocumentOperationError> {
    if let (OwnerResolution::Named, Some(owner)) =
        (decide_owner(owner.is_some(), OwnerCount::None), owner)
    {
        return Ok(owner.to_owned());
    }
    let response = access
        .read(&owners_of)
        .await
        .map_err(|error| DocumentOperationError::Passthrough(error.into()))?;
    let mut owners = rows(&response, collection)
        .iter()
        .filter_map(|row| row["node_did"].as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    owners.sort();
    owners.dedup();
    let count = match owners.len() {
        0 => OwnerCount::None,
        1 => OwnerCount::One,
        _ => OwnerCount::Several,
    };
    match decide_owner(false, count) {
        OwnerResolution::Named | OwnerResolution::Sole => Ok(owners.remove(0)),
        OwnerResolution::RefusedMissing => Err(DocumentOperationError::Missing {
            collection,
            id: id.to_owned(),
            node_did: "any owner this identity can read".to_owned(),
        }),
        OwnerResolution::RefusedAmbiguous => Err(DocumentOperationError::AmbiguousOwner {
            collection,
            id: id.to_owned(),
            owners,
        }),
    }
}

/// The node that owns the Agent `agent_did`.
pub async fn resolve_agent_owner(
    access: &ConfigAccess,
    owner: Option<&str>,
    agent_did: &str,
) -> Result<String, DocumentOperationError> {
    use fields::agent;
    let owners_of = Query::select(Selection::new().field(agent::NODE_DID))
        .filter(Filter::eq(agent::AGENT_DID, agent_did))
        .render();
    resolve_owner(access, "Agent", owner, agent_did, owners_of).await
}

/// The node that owns the Skill `skill_id`.
pub async fn resolve_skill_owner(
    access: &ConfigAccess,
    owner: Option<&str>,
    skill_id: &str,
) -> Result<String, DocumentOperationError> {
    use fields::skill;
    let owners_of = Query::select(Selection::new().field(skill::NODE_DID))
        .filter(Filter::eq(skill::SKILL_ID, skill_id))
        .render();
    resolve_owner(access, "Skill", owner, skill_id, owners_of).await
}
