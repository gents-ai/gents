use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::Tools;
use gents::collection::Collection;
use gents::config_client::{
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, apply_desired_state_plan,
    read_desired_state_record_in_txn,
};
use gents::document_config::ToolServiceRegistry;

pub async fn upsert_tools(node: &EmbeddedNode, document: &Tools) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.tools.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn upsert_tool_service_registry(
    node: &EmbeddedNode,
    document: &ToolServiceRegistry,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::ToolServiceRegistry,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.tool_service.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_tools(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::Tools,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.tools.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = read_desired_state_record_in_txn(txn, Collection::Tools, agent_did, id)
                .await?
                .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

pub async fn delete_tool_service_registry(
    node: &EmbeddedNode,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::ToolServiceRegistry,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.tool_service.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = read_desired_state_record_in_txn(
                txn,
                Collection::ToolServiceRegistry,
                agent_did,
                id,
            )
            .await?
            .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn tools_replacement_revokes_groups_and_service_delete_checks_retained_refs() -> Result<()>
    {
        let node = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(&node).await?;
        for owner in ["did:test:owner", "did:test:other"] {
            gents::ensure_agent_principal(&node, owner).await?;
        }
        let service: ToolServiceRegistry = serde_json::from_value(json!({
            "agent_did":"did:test:owner", "service_id":"service", "hostname":"localhost", "mcp_port":8000
        }))?;
        upsert_tool_service_registry(&node, &service).await?;
        let tools: Tools = serde_json::from_value(json!({
            "agent_did":"did:test:owner", "tools_id":" tools-\"safe\" ",
            "remote":{"services":[{"mcp_service_id":"service", "tool_names":["read"]}]},
            "host":{"files":{"mode":"ReadOnly"}}
        }))?;
        upsert_tools(&node, &tools).await?;
        let mut other = tools.clone();
        other.agent_did = "did:test:other".into();
        assert!(upsert_tools(&node, &other).await.is_err());
        other.remote = None;
        upsert_tools(&node, &other).await?;
        assert!(
            delete_tool_service_registry(&node, &service.agent_did, "service")
                .await
                .is_err()
        );
        let invalid: Tools = serde_json::from_value(json!({
            "agent_did":"did:test:owner", "tools_id":tools.tools_id,
            "subagents":{"target_ids":["missing"],"spawn_enabled":true}
        }))?;
        assert!(upsert_tools(&node, &invalid).await.is_err());
        let mut replaced = tools.clone();
        replaced.host = None;
        replaced.remote = None;
        upsert_tools(&node, &replaced).await?;
        ConfigAccess::transact_local(&node, None, "test.read", |txn| {
            let replaced = &replaced;
            let other = &other;
            Box::pin(async move {
                for expected in [replaced, other] {
                    let (_, value) = read_desired_state_record_in_txn(
                        txn,
                        Collection::Tools,
                        &expected.agent_did,
                        &expected.tools_id,
                    )
                    .await?
                    .unwrap();
                    assert_eq!(serde_json::from_value::<Tools>(value)?, *expected);
                }
                Ok(())
            })
        })
        .await?;
        let mut distinct = replaced.clone();
        distinct.tools_id = replaced.tools_id.trim().to_owned();
        assert_ne!(distinct.tools_id, replaced.tools_id);
        upsert_tools(&node, &distinct).await?;
        assert_eq!(
            delete_tool_service_registry(&node, &service.agent_did, "service").await?,
            1
        );
        assert_eq!(
            delete_tools(&node, &tools.agent_did, &tools.tools_id).await?,
            1
        );
        assert_eq!(
            delete_tools(&node, &tools.agent_did, &tools.tools_id).await?,
            0
        );
        assert_eq!(
            delete_tools(&node, &other.agent_did, &other.tools_id).await?,
            1
        );
        assert_eq!(
            delete_tools(&node, &distinct.agent_did, &distinct.tools_id).await?,
            1
        );
        Ok(())
    }
}
