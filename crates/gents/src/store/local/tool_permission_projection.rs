use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use acp::{NodeACP, NodePermission, Subject, ZanzibarStore};
use sha2::{Digest, Sha256};
use storage::corekv::{Error, Result};
use storage::namespace::Namespace;

const PERMISSIONS: &[NodePermission] = &[
    NodePermission::CollectionGet,
    NodePermission::CollectionPatch,
    NodePermission::MigrationSet,
    NodePermission::ViewAdd,
    NodePermission::DocumentRead,
    NodePermission::DocumentUpdate,
    NodePermission::DacRelationAdd,
    NodePermission::DacRelationDelete,
];

pub(super) struct ToolPermissionProjection {
    node: identity::Did,
    ceiling: crate::tools::surface::ToolCeiling,
}

impl ToolPermissionProjection {
    pub(super) fn new(
        node: &str,
        ceiling: crate::tools::surface::ToolCeiling,
    ) -> std::result::Result<Self, identity::Error> {
        Ok(Self {
            node: identity::Did::new(node)?,
            ceiling,
        })
    }

    pub(super) fn node(&self) -> &identity::Did {
        &self.node
    }

    fn prefix(&self) -> String {
        let mut prefix = String::from("gents-tool-v1:");
        for byte in Sha256::digest(self.node.as_str().as_bytes()) {
            write!(&mut prefix, "{byte:02x}").expect("writing to a string cannot fail");
        }
        prefix.push(':');
        prefix
    }

    /// The source namespace relies on SHA-256 collision resistance to keep
    /// independent Node, document, Agent, and purpose paths distinct.
    fn source(&self, tools: &str, context: &str, agent: &str, purpose: &str) -> String {
        let mut digest = Sha256::new();
        for part in [tools, context, agent, purpose] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        let mut source = self.prefix();
        for byte in digest.finalize() {
            write!(&mut source, "{byte:02x}").expect("writing to a string cannot fail");
        }
        source
    }

    pub(super) async fn update(&self, view: &db::DocumentMutationView) -> Result<()> {
        let agents = documents(view, "Agent").await?;
        let contexts = documents(view, "AgentContext").await?;
        let tools = documents(view, "Tools").await?;
        let datastore_surfaces = documents(view, "DatastoreToolSurface").await?;
        let node = self.node.as_str();
        let mut surfaces = Vec::new();
        for doc in datastore_surfaces {
            let (_, value) = row(doc)?;
            if field(&value, "node_did") == Some(node) {
                if let Ok(surface) = serde_json::from_value(value) {
                    surfaces.push(surface);
                }
            }
        }
        let mut context_by_id = BTreeMap::new();
        for doc in contexts {
            let (id, value) = row(doc)?;
            if field(&value, "node_did") == Some(node) {
                if let Some(key) = field(&value, "context_id") {
                    context_by_id.insert(key.to_owned(), (id, value));
                }
            }
        }
        let mut tools_by_id = BTreeMap::new();
        for doc in tools {
            let (id, value) = row(doc)?;
            if field(&value, "node_did") == Some(node) {
                if let Some(key) = field(&value, "tools_id") {
                    tools_by_id.insert(key.to_owned(), (id, value));
                }
            }
        }
        let mut desired: BTreeMap<&'static str, BTreeMap<String, String>> = BTreeMap::new();
        for doc in agents {
            let (_, value) = row(doc)?;
            if field(&value, "node_did") != Some(node)
                || value.get("enabled").and_then(serde_json::Value::as_bool) == Some(false)
            {
                continue;
            }
            let (Some(agent), Some(context_id)) =
                (field(&value, "agent_did"), field(&value, "context_id"))
            else {
                continue;
            };
            if identity::Did::new(agent).is_err() {
                tracing::warn!(
                    agent_did = agent,
                    "invalid Agent identity cannot receive tool permissions"
                );
                continue;
            }
            let Some((context_doc, context)) = context_by_id.get(context_id) else {
                continue;
            };
            let Some(tools_id) = field(context, "tools_id") else {
                continue;
            };
            let Some((tools_doc, tool_value)) = tools_by_id.get(tools_id) else {
                continue;
            };
            let Ok(tools) = serde_json::from_value::<crate::documents::Tools>(tool_value.clone())
            else {
                continue;
            };
            let Ok(operations) = crate::tools::surface::configured_node_operation_sources(
                &tools,
                &surfaces,
                &self.ceiling,
            ) else {
                continue;
            };
            for operation in operations {
                let source = self.source(tools_doc, context_doc, agent, operation.purpose);
                desired
                    .entry(operation.permission.as_str())
                    .or_default()
                    .insert(source, agent.to_owned());
            }
        }
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            view.sibling(Namespace::Acpstore),
        ));
        let nac = NodeACP::new(Arc::clone(&store));
        nac.load().await.map_err(external)?;
        let prefix = self.prefix();
        for permission in PERMISSIONS {
            let wanted = desired.remove(permission.as_str()).unwrap_or_default();
            let existing = store
                .get_relation_subjects(
                    acp::NODE_POLICY_ID,
                    acp::NODE_RESOURCE_NAME,
                    acp::NODE_OBJECT_ID,
                    permission.as_str(),
                )
                .await
                .map_err(external)?;
            let mut installed = BTreeSet::new();
            for subject in existing {
                if let Subject::EntitySet {
                    resource,
                    object_id,
                    relation,
                } = subject
                {
                    if resource == acp::NODE_RESOURCE_NAME && relation == permission.as_str() {
                        if let Some(source) =
                            decode_source(&object_id).filter(|source| source.starts_with(&prefix))
                        {
                            installed.insert(source);
                        }
                    }
                }
            }
            for source in installed.difference(&wanted.keys().cloned().collect()) {
                nac.change_source_permission(&self.node, *permission, None, "source", source, true)
                    .await
                    .map_err(external)?;
            }
            for (source, agent) in wanted {
                nac.change_source_permission(
                    &self.node,
                    *permission,
                    Some(&source),
                    "actor",
                    &agent,
                    false,
                )
                .await
                .map_err(external)?;
                nac.change_source_permission(
                    &self.node,
                    *permission,
                    None,
                    "source",
                    &source,
                    false,
                )
                .await
                .map_err(external)?;
            }
        }
        Ok(())
    }
}

async fn documents(view: &db::DocumentMutationView, name: &str) -> Result<Vec<document::Document>> {
    let system = view.sibling(Namespace::Systemstore);
    let Some(collection) = db::collection::loader::load_collection_from_systemstore(&system, name)
        .await
        .map_err(external)?
    else {
        return Ok(Vec::new());
    };
    collection
        .get_all_with_datastore_validated(view, &system)
        .await
        .map_err(external)
}

fn row(doc: document::Document) -> Result<(String, serde_json::Value)> {
    let id = doc
        .id()
        .ok_or_else(|| Error::Other("configuration document has no ID".into()))?
        .to_string();
    let mut value = serde_json::to_value(doc.to_map().map_err(external)?).map_err(external)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("_docID");
        object.remove("updated_at");
    }
    Ok((id, value))
}

fn field<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
}

fn decode_source(object_id: &str) -> Option<String> {
    let hex = object_id.strip_prefix("source.")?.as_bytes();
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push(((high << 4) | low) as u8);
    }
    String::from_utf8(bytes).ok()
}

fn external(error: impl std::fmt::Display) -> Error {
    Error::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::graphql::escape_graphql_string;
    use crate::store::identity::NodeIdentity;

    async fn projection_node(
    ) -> anyhow::Result<(tempfile::TempDir, crate::KeyIdentity, Arc<defra_node::Node>)> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime");
        let runtime = crate::KeyIdentity::load_or_create(&path, None)?;
        let key = crate::store::identity::load_file_identity(&path)?.private_key_bytes();
        let projector = Arc::new(
            super::super::runtime_document_projection::RuntimeDocumentProjector::new(
                runtime.did(),
                crate::tools::surface::ToolCeiling::meta_only(),
            )?,
        );
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .with_document_projector(projector)
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        Ok((directory, runtime, node))
    }

    #[tokio::test]
    async fn selected_datastore_query_grants_collection_access_until_removed() -> anyhow::Result<()>
    {
        let (_directory, runtime, node) = projection_node().await?;
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let sibling_binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let agent_signer = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let sibling_value = escape_graphql_string(&sibling_binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let sibling_nonce = escape_graphql_string(&sibling_binding.creation_nonce);
        let owner = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        let entries = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "entries": [{"kind":"query", "tool_name":"find_probe", "collection":"Probe",
                "description":"Find a probe", "fields":["name"]}]
        }))?;
        let datastore = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "datastore_tool_surface_ids": ["surface"]
        }))?;
        for mutation in [
            format!(
                r#"mutation {{ create_DatastoreToolSurface(input: {{node_did:"{node_value}",surface_id:"surface",enabled:true,entries:{entries}}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"tools",datastore:{datastore}}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"context",enabled:true}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{sibling_value}",creation_nonce:"{sibling_nonce}",enabled:true}}) {{_docID}} }}"#
            ),
        ] {
            owner
                .write("test.tool_projection.create", &mutation)
                .await?;
        }
        let agent = identity::Did::new(&binding.agent_did)?;
        let sibling = identity::Did::new(&sibling_binding.agent_did)?;
        let access = crate::ConfigAccess::local_managed_agent(
            node.clone(),
            agent.clone(),
            agent_signer.as_ref(),
        )?;
        let model = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration contract");
        let expected = |name: &str| {
            model
                .tool_operation_cases
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled operation case")["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| source["permission"] == "get-collection")
        };
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionGet)
                .await?,
            expected("datastore-query"),
        );
        assert!(access.collection_version("Node").await?.is_some());
        assert_eq!(
            node.nac_manager()
                .check_permission(&sibling, NodePermission::CollectionGet)
                .await?,
            expected("none"),
        );
        owner.write("test.tool_projection.schema_enable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#)).await?;
        owner.write("test.tool_projection.surface_disable", &format!(r#"mutation {{ update_DatastoreToolSurface(filter: {{node_did:{{_eq:"{node_value}"}},surface_id:{{_eq:"surface"}}}},input:{{enabled:false}}) {{_docID}} }}"#)).await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionGet)
                .await?,
            expected("schema"),
        );
        owner.write("test.tool_projection.schema_disable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#)).await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionGet)
                .await?,
            expected("none"),
        );
        assert!(access.collection_version("Node").await.is_err());
        node.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn selected_schema_tool_grants_and_revokes_only_its_native_source() -> anyhow::Result<()>
    {
        let (_directory, runtime, node) = projection_node().await?;
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let agent_signer = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(agent_signer.did());
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let owner = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        for mutation in [
            format!(
                r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"tools",built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"context",enabled:true}}) {{_docID}} }}"#
            ),
        ] {
            owner
                .write("test.tool_projection.create", &mutation)
                .await?;
        }
        let agent = identity::Did::new(agent_signer.did())?;
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        owner
            .grant_node_permission(&agent, NodePermission::CollectionPatch)
            .await?;
        owner.write("test.tool_projection.schema_disable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#)).await?;
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        owner
            .revoke_node_permission(&agent, NodePermission::CollectionPatch)
            .await?;
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        node.shutdown().await?;
        Ok(())
    }
}
