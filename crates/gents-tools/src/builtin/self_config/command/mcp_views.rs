use super::*;
use crate::mcp::views::{ServicePage, Services};
use gents_session::session::output_render::ordered;
use gents_store::config_client::ConfigAccess;

impl ConfigCommandTool {
    pub(super) async fn mcp_inventory(&self, limit: usize, cursor: Option<&str>) -> Result<String> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let (mut rows, total) = ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.mcp_inventory",
            |txn| {
                Box::pin(async move {
                    let all = txn
                        .read_view(&Services {
                            node_did: &self.node_did,
                            service_id: None,
                            enabled_only: false,
                        })
                        .await?;
                    crate::mcp::registry::validate_services(&all, &self.node_did)?;
                    let page = txn
                        .read_view(&ServicePage {
                            node_did: &self.node_did,
                            cursor,
                            limit: (limit + 1) as u64,
                        })
                        .await?;
                    Ok::<_, anyhow::Error>((page, all.len()))
                })
            },
        )
        .await?;
        let truncated = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = truncated
            .then(|| rows.last().map(|row| &row.service_id))
            .flatten();
        ordered! {
            "resource": "ToolServiceRegistry",
            "items": rows,
            "page": json!({
                "limit": limit,
                "total": total,
                "returned": rows.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            }),
            "note": "Inventory is read-only and does not rebind any agent.",
        }
        .pretty()
    }

    pub(super) async fn mcp_exact_read(&self, id: &str) -> Result<String> {
        let rows = self
            .core
            .access()?
            .read_view(&Services {
                node_did: &self.node_did,
                service_id: Some(id),
                enabled_only: false,
            })
            .await?;
        crate::mcp::registry::validate_services(&rows, &self.node_did)?;
        let document = rows
            .into_iter()
            .next()
            .ok_or(super::super::ops::MissingConfigDocument {
                target: SelfConfigTarget::ToolServiceRegistry,
                id: id.to_owned(),
            })?;
        ordered! {"resource": "ToolServiceRegistry", "document": document}.pretty()
    }
}
