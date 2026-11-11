//! Lean `Identity.DeclaredAccess.admissible`: a pack's declared access policy
//! (a RepositoryPlacement's `readers`, a Task's `outcome_readers`) names only
//! the Agents the pack provisions or the installing actor. The desired-state
//! and outcome owners apply the declarations; this check bounds their subjects.

use anyhow::Result;

use gents_documents::DesiredStateApplyDocument;
use gents_store::collection::Collection;

fn declared_field(collection: Collection) -> Option<&'static str> {
    match collection {
        Collection::RepositoryPlacement => Some("readers"),
        Collection::Task => Some("outcome_readers"),
        _ => None,
    }
}

/// The access policy `document` declares: the field that holds it and the
/// readers it names, or `None` for a collection that declares none.
pub fn declared_readers(
    collection: Collection,
    document: &serde_json::Value,
) -> Option<(&'static str, Vec<&str>)> {
    let field = declared_field(collection)?;
    let readers = document[field]
        .as_array()
        .into_iter()
        .flatten()
        .map(|reader| reader["agent_did"].as_str().unwrap_or_default())
        .collect();
    Some((field, readers))
}

pub(super) fn ensure_admissible(
    documents: &[DesiredStateApplyDocument],
    bindings: &super::super::PackAgentBindings,
    installer: Option<&str>,
) -> Result<()> {
    for document in documents {
        let Some((field, readers)) = declared_readers(document.collection, &document.add) else {
            continue;
        };
        for did in readers {
            anyhow::ensure!(
                bindings.values().any(|binding| binding.agent_did == did) || installer == Some(did),
                "{} {} declares reader {did:?}, which is not an Agent this pack provisions",
                document.collection.graphql_type(),
                field
            );
        }
    }
    Ok(())
}
