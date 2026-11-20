use storage::corekv::{Error, Result};

use super::tool_permission_projection::ToolPermissionProjection;
use crate::config_client::transport_publication::{
    project_transport_document, reconcile_transport_documents,
};

pub struct RuntimeDocumentProjector {
    tools: ToolPermissionProjection,
    startup_plugins: Option<Vec<super::ProjectedPlugin>>,
    projection_rules: Vec<acp::projection::Rule>,
}

impl RuntimeDocumentProjector {
    pub fn new(
        node: std::sync::Arc<dyn crate::identity::NodeIdentity>,
        policy: std::sync::Arc<dyn super::ToolSourcePolicy>,
    ) -> std::result::Result<Self, identity::Error> {
        let mut projection_rules = Vec::new();
        for collection in gents_protocol::schemas::BRANCHABLE_AGENT_COLLECTION_NAMES {
            if !gents_protocol::schemas::is_replicated_protocol_collection(collection) {
                continue;
            }
            if let Some(resource) = gents_protocol::schemas::identity_policy_resource(collection) {
                use acp::projection::TargetKind::{Entity, EntitySet, Wildcard};
                for (relation, target_kind) in [
                    ("reader", Entity),
                    ("reader_manager", Entity),
                    ("local_publication", Entity),
                    ("pending_parent", Entity),
                    ("parent", EntitySet),
                ] {
                    projection_rules.push(acp::projection::Rule {
                        collection: (*collection).to_owned(),
                        resource: resource.to_owned(),
                        relation: relation.to_owned(),
                        target_kind,
                    });
                }
                if collection == &gents_protocol::schemas::PEER_REGISTRY_NAME {
                    for (relation, target_kind) in [("reader", Wildcard), ("writer", Entity)] {
                        projection_rules.push(acp::projection::Rule {
                            collection: (*collection).to_owned(),
                            resource: resource.to_owned(),
                            relation: relation.to_owned(),
                            target_kind,
                        });
                    }
                }
            }
        }
        Ok(Self {
            tools: ToolPermissionProjection::new(node, policy)?,
            startup_plugins: None,
            projection_rules,
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
        if project_transport_document(ctx, collection, doc_id).await? {
            ctx.notify_access_changed(collection, doc_id).await?;
        }
        Ok(())
    }

    async fn reconcile_transport(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        for (collection, doc_id) in reconcile_transport_documents(ctx).await? {
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
    fn genesis_read_access(
        &self,
        collection: &str,
        policy: &str,
        resource: &str,
    ) -> Option<db::GenesisReadAccess> {
        if policy != gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID {
            return None;
        }
        if collection == gents_protocol::schemas::PEER_REGISTRY_NAME && resource == "discovery" {
            return Some(db::GenesisReadAccess::Public);
        }
        (resource == "enrollment_records"
            && matches!(
                collection,
                "NetworkEnrollmentRequest"
                    | "NetworkEnrollmentDecision"
                    | "NetworkAuthorizationRevision"
                    | "NetworkEnrollmentRouteReceipt"
                    | "EnrollmentRenewalIntent"
                    | "EnrollmentRenewalOffer"
            ))
        .then_some(db::GenesisReadAccess::AddressedBy("initial_reader_did"))
    }

    fn projection_rules(&self) -> &[acp::projection::Rule] {
        &self.projection_rules
    }
    async fn source_changed(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        self.require_node(ctx)?;
        self.tools.update(ctx).await
    }

    async fn reconcile(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        self.reconcile_runtime(ctx).await
    }

    async fn project_related(
        &self,
        ctx: &db::DocumentMutationContext<'_>,
        collection: &str,
        _doc_id: &str,
    ) -> Result<()> {
        if collection == gents_protocol::schemas::NODE_NAME {
            self.require_node(ctx)?;
            self.reconcile_transport(ctx).await?;
        }
        Ok(())
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
