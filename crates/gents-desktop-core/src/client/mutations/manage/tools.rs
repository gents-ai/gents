use anyhow::Result;
use gents::documents::ToolServiceRegistry;
use gents::store::collection::Collection;
#[cfg(test)]
use gents::store::config_client::read_desired_state_record_in_txn;
use gents::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::Tools;

pub async fn upsert_tools_on(access: &ConfigAccess, document: &Tools) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.tools.save", plan).await
}

pub async fn upsert_tool_service_registry_on(
    access: &ConfigAccess,
    document: &ToolServiceRegistry,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::ToolServiceRegistry,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.tool_service.save", plan).await
}

pub async fn delete_tools_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.tools.delete",
        Collection::Tools,
        node_did,
        id,
    )
    .await
}

pub async fn delete_tool_service_registry_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.tool_service.delete",
        Collection::ToolServiceRegistry,
        node_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use serde_json::json;

    #[tokio::test]
    async fn tools_replacement_revokes_groups_and_service_delete_checks_retained_refs() -> Result<()>
    {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let owner = runtime.node_did();
        let other_owner = runtime.publish_foreign_node().await;
        let service: ToolServiceRegistry = serde_json::from_value(json!({
            "node_did":owner, "service_id":"service", "hostname":"localhost", "mcp_port":8000
        }))?;
        upsert_tool_service_registry_on(&access, &service).await?;
        let tools: Tools = serde_json::from_value(json!({
            "node_did":owner, "tools_id":" tools-\"safe\" ",
            "remote":{"services":[{"mcp_service_id":"service", "tool_names":["read"]}]},
            "host":{"files":{"mode":"ReadOnly"}}
        }))?;
        upsert_tools_on(&access, &tools).await?;
        let mut other = tools.clone();
        other.node_did = other_owner.as_str().to_owned();
        assert!(upsert_tools_on(&access, &other).await.is_err());
        other.remote = None;
        upsert_tools_on(&access, &other).await?;
        assert!(
            delete_tool_service_registry_on(&access, &service.node_did, "service")
                .await
                .is_err()
        );
        let invalid: Tools = serde_json::from_value(json!({
            "node_did":owner, "tools_id":tools.tools_id,
            "agents":{"target_ids":["missing"],"enabled":true}
        }))?;
        assert!(upsert_tools_on(&access, &invalid).await.is_err());
        let mut replaced = tools.clone();
        replaced.host = None;
        replaced.remote = None;
        upsert_tools_on(&access, &replaced).await?;
        access
            .transact_write("test.read", |txn| {
                let replaced = &replaced;
                let other = &other;
                Box::pin(async move {
                    for expected in [replaced, other] {
                        let (_, value) = read_desired_state_record_in_txn(
                            txn,
                            Collection::Tools,
                            &expected.node_did,
                            &expected.tools_id,
                        )
                        .await?
                        .unwrap();
                        assert_eq!(serde_json::from_value::<Tools>(value)?, *expected);
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let mut distinct = replaced.clone();
        distinct.tools_id = replaced.tools_id.trim().to_owned();
        assert_ne!(distinct.tools_id, replaced.tools_id);
        upsert_tools_on(&access, &distinct).await?;
        assert_eq!(
            delete_tool_service_registry_on(&access, &service.node_did, "service").await?,
            1
        );
        assert_eq!(
            delete_tools_on(&access, &tools.node_did, &tools.tools_id).await?,
            1
        );
        assert_eq!(
            delete_tools_on(&access, &tools.node_did, &tools.tools_id).await?,
            0
        );
        assert_eq!(
            delete_tools_on(&access, &other.node_did, &other.tools_id).await?,
            1
        );
        assert_eq!(
            delete_tools_on(&access, &distinct.node_did, &distinct.tools_id).await?,
            1
        );
        runtime.shutdown().await;
        Ok(())
    }
}
