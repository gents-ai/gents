use storage::corekv::{Error, Result};

use super::tool_permission_projection::ToolPermissionProjection;
use crate::store::config_client::transport_publication::{
    project_transport_document, reconcile_transport_documents,
};

pub(super) struct RuntimeDocumentProjector {
    tools: ToolPermissionProjection,
    startup_plugins: Option<Vec<super::ProjectedPlugin>>,
}

impl RuntimeDocumentProjector {
    pub(super) fn new(
        node: std::sync::Arc<dyn crate::store::identity::NodeIdentity>,
        policy: std::sync::Arc<dyn super::ToolSourcePolicy>,
    ) -> std::result::Result<Self, identity::Error> {
        Ok(Self {
            tools: ToolPermissionProjection::new(node, policy)?,
            startup_plugins: None,
        })
    }

    pub(super) fn with_startup_plugins(
        mut self,
        plugins: Vec<super::ProjectedPlugin>,
    ) -> Self {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use acp::{Subject, ZanzibarStore};
    use storage::namespace::Namespace;

    use super::RuntimeDocumentProjector;
    use crate::store::identity::NodeIdentity;

    #[tokio::test]
    async fn installed_projector_links_pending_transport_and_preserves_revocation(
    ) -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime");
        let runtime = crate::KeyIdentity::load_or_create(&path, None)?;
        let key = crate::store::identity::load_file_identity(&path)?.private_key_bytes();
        let projector = RuntimeDocumentProjector::new(
            Arc::new(crate::KeyIdentity::load_or_create(&path, None)?),
            Arc::new(crate::tools::surface::ToolCeiling::meta_only()),
        )?;
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .with_document_projector(Arc::new(projector))
                .build()
                .await?,
        );
        let actor = identity::Did::new(runtime.did())?;
        let context =
            defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
        node.add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        node.add_collection(gents_protocol::schemas::NETWORK)
            .await?;
        node.add_collection(gents_protocol::schemas::NODE).await?;
        let access = crate::ConfigAccess::local_as(node.clone(), actor.clone());
        let did = crate::store::graphql::escape_graphql_string(runtime.did());
        let network = access
            .write(
                "test.projector.network",
                &format!(r#"mutation {{ create_Network(input: {{network_id: "network", admin_did: "{did}"}}) {{_docID}} }}"#),
            )
            .await?;
        let network = crate::store::graphql::created_doc_id(&network, "Network")?;
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_replica_grant_cases;
        let modeled_pending = |name: &str| {
            cases
                .iter()
                .find(|case| case["name"] == name)
                .and_then(|case| case["pending"].as_bool())
                .expect("modeled transport publication case")
        };
        let relation = |name: &'static str, subject: Subject, network: String| {
            let node = node.clone();
            async move {
                let txn = node.database().new_txn(true).await?;
                let view = txn.datastore()?;
                let tuples = acp::zanzibar::store::TransactionZanzibarStore::new(
                    view.sibling(Namespace::Acpstore),
                );
                let present = tuples
                    .has_relationship(
                        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                        "enrollment_records",
                        &network,
                        name,
                        &subject,
                    )
                    .await?;
                drop(tuples);
                drop(view);
                txn.discard()?;
                Ok::<_, anyhow::Error>(present)
            }
        };
        assert_eq!(
            relation(
                "pending_parent",
                Subject::entity(actor.clone()),
                network.clone()
            )
            .await?,
            modeled_pending("pending")
        );
        let parent = access
            .write(
                "test.projector.node",
                &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
            )
            .await?;
        let parent = crate::store::graphql::created_doc_id(&parent, "Node")?;
        let parent_subject = Subject::entity_set("nodes", parent.clone(), "");
        assert!(relation("parent", parent_subject.clone(), network.clone()).await?);
        assert_eq!(
            relation(
                "pending_parent",
                Subject::entity(actor.clone()),
                network.clone()
            )
            .await?,
            modeled_pending("linked")
        );
        access
            .delete_resource_relationship("Network", &network, "parent", "nodes", &parent)
            .await?;
        access
            .write(
                "test.projector.network_update",
                &format!(r#"mutation {{ update_Network(docID: "{}", input: {{display_name: "updated"}}) {{_docID}} }}"#, crate::store::graphql::escape_graphql_string(&network)),
            )
            .await?;
        assert!(!relation("parent", parent_subject, network).await?);
        node.shutdown().await?;
        Ok(())
    }
}
