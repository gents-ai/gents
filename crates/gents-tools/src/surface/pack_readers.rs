use anyhow::{Context, Result};
use gents_protocol::operations::pack::{PackActor, ReaderAction, RootReader};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

use super::ToolRootReader;

/// What an install did, or reports, about the reader one shipped Agent needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRootReader {
    pub agent: String,
    /// The published root, or the unpublished tool root.
    pub root: String,
    pub action: ReaderAction,
}

/// Lean `Pack.Operations.readerAction` for each Agent the installed pack
/// `coordinate` ships with host tools: `actor`'s install grants reader on the
/// published root covering its tool root, or reports the step.
pub async fn pack_root_readers(
    access: &ConfigAccess,
    node_did: &str,
    coordinate: &str,
    actor: PackActor,
) -> Result<Vec<PackRootReader>> {
    let agents: Vec<String> =
        gents_pack::pack::installed_pack_documents(access, node_did, coordinate)
            .await?
            .into_iter()
            .filter(|(collection, _)| *collection == Collection::Agent)
            .map(|(_, agent)| agent)
            .collect();
    let mut readers = Vec::new();
    for (agent, reader) in super::agents_tool_root_readers(access, node_did, &agents).await? {
        let (kind, root) = match reader {
            ToolRootReader::NotNeeded => continue,
            ToolRootReader::Published(root) => (RootReader::Published, root),
            ToolRootReader::Unpublished(root) => (RootReader::Unpublished, root),
        };
        let action = gents_pack::operations::pack::reader_action(actor, kind);
        if action == ReaderAction::Grant {
            super::grant_workspace_root_reader(access, &root, &agent)
                .await
                .with_context(|| format!("granting {agent} reader on {root}"))?;
        }
        readers.push(PackRootReader {
            agent,
            root,
            action,
        });
    }
    Ok(readers)
}
