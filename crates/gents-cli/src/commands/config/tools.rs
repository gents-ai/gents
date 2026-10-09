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
    crate::print_json(&serde_json::to_value(AgentTargetDocument {
        target_id: args.target_id,
        node_did: args.node_did,
        target_node_did: args.target_node_did,
        agent_id: args.agent_id,
        name: args.name,
        description: args.description,
        tags: Vec::new(),
    })?)
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
        .transact("cli.tools.set", move |txn| {
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
    crate::print_json(
        &json!({"doc_id": doc_id, "tools_id": tools.tools_id, "node_did": tools.node_did}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_tools_use_canonical_defaults_and_preserve_explicit_selection() {
        let (tools, plan) = tools_plan(br#"{"node_did":"owner","tools_id":"main","built_ins":{"enable_goal_tools":true},"agents":{"target_ids":["worker"]}}"#).unwrap();
        assert_eq!(tools.agents.unwrap().target_ids, ["worker"]);
        assert_eq!(plan.documents().len(), 1);
        assert_eq!(
            plan.documents()[0].update["built_ins"]["enable_goal_tools"],
            true
        );
    }
    #[test]
    fn invalid_or_retired_fields_cannot_enter_tools_plan() {
        for input in [
            r#"{"node_did":"owner","tools_id":"main","enable_bash":true}"#,
            r#"{"tools_id":"main"}"#,
            r#"{"node_did":"owner","tools_id":"main","host":{"unknown":true}}"#,
        ] {
            assert!(tools_plan(input.as_bytes()).is_err(), "{input}");
        }
    }
}
