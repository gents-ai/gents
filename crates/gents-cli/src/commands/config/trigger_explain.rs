use anyhow::{Context, Result};
use gents_documents::trigger_explain::explain_trigger_in_txn;

use crate::cli::ConfigTriggerExplainArgs;
use crate::{print_json, resolve_config_access};

pub(crate) async fn config_trigger_explain(args: ConfigTriggerExplainArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for config trigger explain")?;
    let node_did = super::binding::resolve_target_node_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let explanation = access
        .transact_readonly("trigger.explain", |txn| {
            let node_did = node_did.as_str();
            let trigger_id = args.trigger_id.as_str();
            let source_doc = args.source_doc.as_deref();
            Box::pin(explain_trigger_in_txn(
                txn, node_did, trigger_id, source_doc,
            ))
        })
        .await?;
    print_json(&explanation)
}
