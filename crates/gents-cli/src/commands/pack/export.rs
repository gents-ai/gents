use anyhow::{Context, Result};
use gents::pack::authoring::{export_pack, selectable_kind, ExportSelection, SELECTABLE_KINDS};

use crate::cli::PackExportArgs;

pub(crate) async fn export(args: PackExportArgs) -> Result<()> {
    let mut selection = ExportSelection {
        collections: args.collection.into_iter().collect(),
        tags: args.tag.into_iter().collect(),
        ..ExportSelection::default()
    };
    for pair in &args.select {
        let kind = pair
            .split_once('=')
            .and_then(|(kind, id)| Some((selectable_kind(kind)?, id)))
            .filter(|(_, id)| !id.is_empty())
            .with_context(|| {
                format!(
                    "--select takes KIND=ID, not {pair:?}; kinds: {}",
                    SELECTABLE_KINDS
                        .iter()
                        .map(|(word, _)| *word)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        selection.documents.insert((kind.0, kind.1.to_owned()));
    }
    let dir = args
        .out
        .unwrap_or_else(|| gents::pack::authoring::pack_name(&args.name).into());
    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let report = export_pack(
        &access,
        &owner,
        &dir,
        &args.name,
        &args.namespace,
        &selection,
    )
    .await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    let report = report?;
    let failed = !report.check.problems.is_empty();
    crate::print_json(&report)?;
    anyhow::ensure!(!failed, "the exported pack failed its check");
    Ok(())
}
