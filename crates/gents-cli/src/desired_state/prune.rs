use super::{diff::canonical_records, DesiredStateManifest, DocRef};
use anyhow::Result;
use gents::document_config::ConfigReferences;

pub(crate) fn prune_safe_deletes(
    desired: &DesiredStateManifest,
    live: &DesiredStateManifest,
) -> Result<Vec<DocRef>> {
    anyhow::ensure!(
        desired.agent_principal.agent_did == live.agent_principal.agent_did,
        "cannot prune configuration across principals"
    );
    let desired = canonical_records(desired)?;
    let records = canonical_records(live)?;
    let references = ConfigReferences::from_documents(
        &live.agent_principal.agent_did,
        records
            .iter()
            .map(|((collection, _), value)| (*collection, value.clone())),
    )?;
    references.validate()?;
    let mut deletes = records
        .keys()
        .filter(|key| !desired.contains_key(*key))
        .filter(|(collection, id)| references.validate_removal(*collection, id).is_ok())
        .map(|(collection, id)| DocRef {
            collection: *collection,
            id: id.clone(),
        })
        .collect::<Vec<_>>();
    deletes.sort_by_key(|doc| (std::cmp::Reverse(doc.collection), doc.id.clone()));
    Ok(deletes)
}
