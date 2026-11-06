use storage::corekv::{Error, Result};

use super::tool_permission_projection::ToolPermissionProjection;
use crate::store::config_client::transport_publication::{
    project_transport_document, reconcile_transport_documents,
};
use crate::tools::surface::ToolCeiling;

pub(super) struct RuntimeDocumentProjector {
    tools: ToolPermissionProjection,
}

impl RuntimeDocumentProjector {
    pub(super) fn new(
        node: &str,
        ceiling: ToolCeiling,
    ) -> std::result::Result<Self, identity::Error> {
        Ok(Self {
            tools: ToolPermissionProjection::new(node, ceiling)?,
        })
    }

    async fn reconcile_runtime(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        self.require_node(ctx)?;
        self.tools.update(ctx.view).await?;
        reconcile_transport_documents(ctx.view, ctx.document_acp, ctx.node_did).await
    }

    async fn project_runtime(
        &self,
        ctx: &db::DocumentMutationContext<'_>,
        collection: &str,
        doc_id: &str,
    ) -> Result<()> {
        self.require_node(ctx)?;
        if matches!(collection, "Agent" | "AgentContext" | "Tools") {
            self.tools.update(ctx.view).await?;
        }
        if collection == gents_protocol::schemas::NODE_NAME {
            reconcile_transport_documents(ctx.view, ctx.document_acp, ctx.node_did).await?;
        } else {
            project_transport_document(
                ctx.view,
                ctx.document_acp,
                ctx.node_did,
                collection,
                doc_id,
            )
            .await?;
        }
        Ok(())
    }

    fn require_node(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        if self.tools.node() != ctx.node_did {
            return Err(Error::Other(
                "runtime projector Node identity mismatch".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl db::DocumentMutationProjector for RuntimeDocumentProjector {
    async fn reconcile(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        self.reconcile_runtime(ctx).await
    }

    async fn project(
        &self,
        ctx: &db::DocumentMutationContext<'_>,
        collection: &str,
        doc_id: &str,
    ) -> Result<()> {
        self.project_runtime(ctx, collection, doc_id).await
    }
}
