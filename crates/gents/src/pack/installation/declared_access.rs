//! Lean `Identity.DeclaredAccess.admissible`: a pack's declared access policy
//! (a RepositoryPlacement's `readers`, a Task's `outcome_readers`) names only
//! the Agents the pack provisions or the installing actor. The desired-state
//! and outcome owners apply the declarations; this check bounds their subjects.

use anyhow::Result;

use crate::store::config_client::DesiredStateApplyDocument;
use crate::Collection;

fn declared_field(collection: Collection) -> Option<&'static str> {
    match collection {
        Collection::RepositoryPlacement => Some("readers"),
        Collection::Task => Some("outcome_readers"),
        _ => None,
    }
}

pub(super) fn ensure_admissible(
    documents: &[DesiredStateApplyDocument],
    bindings: &super::super::PackAgentBindings,
    installer: Option<&str>,
) -> Result<()> {
    for document in documents {
        let Some(field) = declared_field(document.collection) else {
            continue;
        };
        for reader in document.add[field].as_array().into_iter().flatten() {
            let did = reader["agent_did"].as_str().unwrap_or_default();
            anyhow::ensure!(
                bindings.values().any(|binding| binding.agent_did == did)
                    || installer == Some(did),
                "{} {} declares reader {did:?}, which is not an Agent this pack provisions",
                document.collection.graphql_type(),
                field
            );
        }
    }
    Ok(())
}
