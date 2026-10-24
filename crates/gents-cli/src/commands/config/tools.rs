use crate::cli::{AgentTargetEntryArgs, ToolsSetArgs};
use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents::document_config::{AgentTargetDocument, Tools};
use gents::Collection;
use serde_json::json;

pub(super) fn agent_target_entry_command(args: AgentTargetEntryArgs) -> Result<()> {
    Ok(crate::print_json(&serde_json::to_value(
        AgentTargetDocument {
            target_id: args.target_id,
            node_did: args.node_did,
            target_node_did: args.target_node_did,
            agent_id: args.agent_id,
            name: args.name,
            description: args.description,
            tags: Vec::new(),
        },
    )?)?)
}

fn tools_plan(contents: &[u8]) -> Result<(Tools, DesiredStateApplyPlan)> {
    let tools: Tools =
        serde_json::from_slice(contents).context("decoding canonical Tools document")?;
    tools.validate()?;
    let value = serde_json::to_value(&tools)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: value.clone(),
        update: value,
    }])?;
    Ok((tools, plan))
}

pub(super) async fn tools_set(args: ToolsSetArgs) -> Result<()> {
    let (tools, plan) = tools_plan(
        &std::fs::read(&args.file)
            .with_context(|| format!("reading Tools document {}", args.file.display()))?,
    )?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let owner = tools.node_did.clone();
    let id = tools.tools_id.clone();
    let (plan, owner, id) = (&plan, &owner, &id);
    let doc_id = access
        .transact_write("cli.tools.set", move |txn| {
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                let (doc_id, _) =
                    read_desired_state_record_in_txn(txn, Collection::Tools, owner, id)
                        .await?
                        .context("committed Tools document is missing")?;
                Ok(doc_id)
            })
        })
        .await?;
    Ok(crate::print_json(
        &json!({"doc_id": doc_id, "tools_id": tools.tools_id, "node_did": tools.node_did}),
    )?)
}
