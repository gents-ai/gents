use storage::corekv::{Error, Result};

use super::tool_permission_projection::ToolPermissionProjection;
use crate::config_client::transport_publication::{
    project_transport_document, reconcile_transport_documents,
};

pub struct RuntimeDocumentProjector {
    tools: ToolPermissionProjection,
    startup_plugins: Option<Vec<super::ProjectedPlugin>>,
}

impl RuntimeDocumentProjector {
    pub fn new(
        node: std::sync::Arc<dyn crate::identity::NodeIdentity>,
        policy: std::sync::Arc<dyn super::ToolSourcePolicy>,
    ) -> std::result::Result<Self, identity::Error> {
        Ok(Self {
            tools: ToolPermissionProjection::new(node, policy)?,
            startup_plugins: None,
        })
    }

    pub(super) fn with_startup_plugins(mut self, plugins: Vec<super::ProjectedPlugin>) -> Self {
        self.startup_plugins = Some(plugins);
        self
    }

    async fn reconcile_runtime(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        self.require_node(ctx)?;
        if let Some(installed) = &self.startup_plugins {
            self.tools.reconcile_installed(ctx, installed).await?;
        }
        self.tools.update(ctx).await?;
        self.reconcile_transport(ctx).await
    }

    async fn project_runtime(
        &self,
        ctx: &db::DocumentMutationContext<'_>,
        collection: &str,
        doc_id: &str,
    ) -> Result<()> {
        self.require_node(ctx)?;
        if matches!(
            collection,
            "Agent" | "AgentContext" | "Tools" | "DatastoreToolSurface" | "PackInstallation"
        ) {
            self.tools.update(ctx).await?;
        }
        if collection == gents_protocol::schemas::NODE_NAME {
            self.reconcile_transport(ctx).await?;
        } else if project_transport_document(
            ctx.view,
            ctx.document_acp,
            ctx.node_did,
            collection,
            doc_id,
        )
        .await?
        {
            ctx.notify_access_changed(collection, doc_id).await?;
        }
        Ok(())
    }

    async fn reconcile_transport(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        for (collection, doc_id) in
            reconcile_transport_documents(ctx.view, ctx.document_acp, ctx.node_did).await?
        {
            ctx.notify_access_changed(&collection, &doc_id).await?;
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
    async fn source_changed(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        self.require_node(ctx)?;
        self.tools.update(ctx).await
    }

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
