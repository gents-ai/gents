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
    NodePermission::DocumentDelete,
    NodePermission::DacRelationAdd,
    NodePermission::DacRelationDelete,
];

pub(super) struct ToolPermissionProjection {
    node: identity::Did,
    runtime: Arc<dyn crate::store::identity::NodeIdentity>,
    ceiling: crate::tools::surface::ToolCeiling,
}

impl ToolPermissionProjection {
    pub(super) fn new(
        runtime: Arc<dyn crate::store::identity::NodeIdentity>,
        ceiling: crate::tools::surface::ToolCeiling,
    ) -> std::result::Result<Self, identity::Error> {
        Ok(Self {
            node: identity::Did::new(runtime.did())?,
            runtime,
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

    pub(super) async fn reconcile_installed(
        &self,
        view: &db::DocumentMutationView,
        installed: &[crate::plugin::store::InstalledPlugin],
    ) -> Result<()> {
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            view.sibling(Namespace::Acpstore),
        ));
        let nac = NodeACP::new(Arc::clone(&store));
        nac.load().await.map_err(external)?;
        let permission = NodePermission::DocumentDelete;
        let node = self.node.as_str();
        let index = super::plugin_delete_source::index(node);
        let mut wanted = BTreeSet::new();
        for plugin in installed {
            if plugin
                .declaration
                .data
                .as_ref()
                .is_some_and(|data| data.ops.iter().any(|op| op == "delete"))
            {
                let coordinate = format!("{}/{}", plugin.namespace, plugin.name);
                wanted.insert(super::plugin_delete_source::root(node, &coordinate, None));
                wanted.insert(super::plugin_delete_source::root(
                    node,
                    &coordinate,
                    Some(&plugin.digest),
                ));
            }
        }
        let indexed = store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                &super::plugin_delete_source::object(&index),
                permission.as_str(),
            )
            .await
            .map_err(external)?;
        for subject in indexed {
            if let Subject::EntitySet {
                resource,
                object_id,
                relation,
            } = subject
            {
                if resource == acp::NODE_RESOURCE_NAME && relation == permission.as_str() {
                    if let Some(root) = decode_source(&object_id).filter(|root| {
                        root.starts_with(&super::plugin_delete_source::root_prefix(node))
                    }) {
                        if !wanted.contains(&root) {
                            nac.change_source_permission(
                                &self.node, permission, None, "source", &root, true,
                            )
                            .await
                            .map_err(external)?;
                        }
                    }
                }
            }
        }
        for root in wanted {
            nac.change_source_permission(
                &self.node,
                permission,
                Some(&index),
                "source",
                &root,
                false,
            )
            .await
            .map_err(external)?;
            nac.change_source_permission(&self.node, permission, None, "source", &root, false)
                .await
                .map_err(external)?;
        }
        Ok(())
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
        let mut plugin_desired: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
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
            let Some(nonce) = field(&value, "creation_nonce") else {
                continue;
            };
            let binding = gents_protocol::agent_identity::AgentIdentityBinding {
                agent_did: agent.to_owned(),
                creation_nonce: nonce.to_owned(),
            };
            if let Err(error) =
                crate::store::identity::resolve_agent_identity(self.runtime.as_ref(), &binding)
            {
                tracing::warn!(
                    agent_did = agent,
                    error = %error,
                    "unmanaged Agent identity cannot receive tool permissions"
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
            if let Ok(plugins) =
                crate::tools::surface::configured_plugin_refs(&tools, &self.ceiling)
            {
                for plugin in plugins {
                    let root = super::plugin_delete_source::root(
                        node,
                        &plugin.plugin,
                        plugin.digest.as_deref(),
                    );
                    let member = super::plugin_delete_source::member(
                        node,
                        &root,
                        tools_doc,
                        context_doc,
                        agent,
                    );
                    plugin_desired
                        .entry(root)
                        .or_default()
                        .insert(member, agent.to_owned());
                }
            }
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
        self.update_plugins(&nac, &store, plugin_desired).await?;
        Ok(())
    }

    async fn update_plugins(
        &self,
        nac: &NodeACP<acp::zanzibar::store::TransactionZanzibarStore>,
        store: &acp::zanzibar::store::TransactionZanzibarStore,
        mut desired: BTreeMap<String, BTreeMap<String, String>>,
    ) -> Result<()> {
        let node = self.node.as_str();
        let permission = NodePermission::DocumentDelete;
        let relation = permission.as_str();
        let index = super::plugin_delete_source::object(&super::plugin_delete_source::index(node));
        let roots = store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                &index,
                relation,
            )
            .await
            .map_err(external)?;
        let active = store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                acp::NODE_OBJECT_ID,
                relation,
            )
            .await
            .map_err(external)?;
        for subject in roots {
            let Subject::EntitySet {
                resource,
                object_id,
                relation: child_relation,
            } = subject
            else {
                continue;
            };
            if resource != acp::NODE_RESOURCE_NAME || child_relation != relation {
                continue;
            }
            let Some(root) = decode_source(&object_id)
                .filter(|s| s.starts_with(&super::plugin_delete_source::root_prefix(node)))
            else {
                continue;
            };
            let root_subject =
                Subject::entity_set(acp::NODE_RESOURCE_NAME, object_id.clone(), relation);
            let wanted = if active.contains(&root_subject) {
                desired.remove(&root).unwrap_or_default()
            } else {
                BTreeMap::new()
            };
            let children = store
                .get_relation_subjects(
                    acp::NODE_POLICY_ID,
                    acp::NODE_RESOURCE_NAME,
                    &object_id,
                    relation,
                )
                .await
                .map_err(external)?;
            for child in children {
                let Subject::EntitySet {
                    resource,
                    object_id: child_object,
                    relation: child_relation,
                } = child
                else {
                    continue;
                };
                if resource != acp::NODE_RESOURCE_NAME || child_relation != relation {
                    continue;
                }
                let Some(member) = decode_source(&child_object)
                    .filter(|s| s.starts_with(&super::plugin_delete_source::member_prefix(node)))
                else {
                    continue;
                };
                if !wanted.contains_key(&member) {
                    let actors = store
                        .get_relation_subjects(
                            acp::NODE_POLICY_ID,
                            acp::NODE_RESOURCE_NAME,
                            &child_object,
                            relation,
                        )
                        .await
                        .map_err(external)?;
                    for actor in actors {
                        if let Subject::Entity(actor) = actor {
                            nac.change_source_permission(
                                &self.node,
                                permission,
                                Some(&member),
                                "actor",
                                actor.as_str(),
                                true,
                            )
                            .await
                            .map_err(external)?;
                        }
                    }
                    nac.change_source_permission(
                        &self.node,
                        permission,
                        Some(&root),
                        "source",
                        &member,
                        true,
                    )
                    .await
                    .map_err(external)?;
                }
            }
            for (member, agent) in wanted {
                nac.change_source_permission(
                    &self.node,
                    permission,
                    Some(&member),
                    "actor",
                    &agent,
                    false,
                )
                .await
                .map_err(external)?;
                nac.change_source_permission(
                    &self.node,
                    permission,
                    Some(&root),
                    "source",
                    &member,
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
    for pair in hex.as_chunks::<2>().0 {
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
                Arc::new(crate::KeyIdentity::load_or_create(&path, None)?),
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
    async fn user_signed_configuration_cannot_grant_schema_to_foreign_identity(
    ) -> anyhow::Result<()> {
        use crate::store::config_client::{ConfigAccess, GraphqlEndpoint};
        use serde_json::json;

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime");
        let runtime = crate::KeyIdentity::load_or_create(&path, None)?;
        let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let key = crate::store::identity::load_file_identity(&path)?.private_key_bytes();
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let projector = Arc::new(
            super::super::runtime_document_projection::RuntimeDocumentProjector::new(
                Arc::new(crate::KeyIdentity::load_or_create(&path, None)?),
                crate::tools::surface::ToolCeiling::meta_only(),
            )?,
        );
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .with_document_projector(projector)
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        let user_did = identity::Did::new(user.did())?;
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
        ] {
            owner.grant_node_permission(&user_did, permission).await?;
        }
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        for (collection, mutation, input) in [
            (crate::Collection::Tools, "mutation($input: ToolsMutationInputArg!) { create_Tools(input: $input) { _docID _version { cid } } }", json!({"node_did":runtime.did(),"tools_id":"tools","built_ins":{"enable_schema_tool":true}})),
            (crate::Collection::AgentContext, "mutation($input: AgentContextMutationInputArg!) { create_AgentContext(input: $input) { _docID _version { cid } } }", json!({"node_did":runtime.did(),"context_id":"context","tools_id":"tools"})),
            (crate::Collection::Agent, "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }", json!({"node_did":runtime.did(),"agent_did":user.did(),"creation_nonce":binding.creation_nonce,"context_id":"context","enabled":true})),
        ] {
            access.transact_write("test.user_signed_tool_configuration", |txn| {
                let input = &input;
                Box::pin(async move { txn.execute_desired_state_mutation(mutation, &json!({"input":input}), collection, true).await })
            }).await?;
        }
        let cases = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let allowed = cases
            .managed_tool_identity_cases
            .iter()
            .find(|case| case["name"] == "foreign")
            .unwrap()["allowed"]
            .as_bool()
            .unwrap();
        assert_eq!(
            node.nac_manager()
                .check_permission(&user_did, NodePermission::CollectionPatch)
                .await?,
            allowed
        );
        for (name, nonce) in [("missing", None), ("malformed", Some("not-a-nonce"))] {
            let foreign = crate::KeyIdentity::load_or_create(directory.path().join(name), None)?;
            let mut input = json!({"node_did":runtime.did(),"agent_did":foreign.did(),
                "context_id":"context","enabled":true});
            if let Some(nonce) = nonce {
                input["creation_nonce"] = json!(nonce);
            }
            access.transact_write("test.user_signed_invalid_agent", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                        &json!({"input":input}), crate::Collection::Agent, true,
                    ).await
                })
            }).await?;
            assert_eq!(
                node.nac_manager()
                    .check_permission(
                        &identity::Did::new(foreign.did())?,
                        NodePermission::CollectionPatch
                    )
                    .await?,
                allowed
            );
        }
        // Install a legacy source to exercise cleanup after upgrading the projector.
        let projector = ToolPermissionProjection::new(
            Arc::new(crate::KeyIdentity::load_or_create(&path, None)?),
            crate::tools::surface::ToolCeiling::meta_only(),
        )?;
        let legacy_source = format!("{}legacy", projector.prefix());
        let txn = node.database().new_txn(false).await?;
        let view = txn.datastore()?.sibling(Namespace::Acpstore);
        let node_did = identity::Did::new(runtime.did())?;
        for (parent_source, subject_kind, subject) in [
            (Some(legacy_source.as_str()), "actor", user.did()),
            (None, "source", legacy_source.as_str()),
        ] {
            assert!(node
                .nac_manager()
                .change_source_permission_in_txn(
                    view.clone(),
                    &node_did,
                    &db::nac::SourcePermissionChange {
                        permission: NodePermission::CollectionPatch,
                        parent_source,
                        subject_kind,
                        subject,
                        delete: false,
                    },
                )
                .await?
                .is_some());
        }
        drop(view);
        txn.commit().await?;
        assert!(
            node.nac_manager()
                .check_permission(&user_did, NodePermission::CollectionPatch)
                .await?
        );
        owner
            .grant_node_permission(&user_did, NodePermission::CollectionPatch)
            .await?;
        let input = json!({"node_did":runtime.did(),"agent_did":binding.agent_did,
            "creation_nonce":binding.creation_nonce,"context_id":"context","enabled":true});
        access.transact_write("test.user_signed_managed_agent", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_desired_state_mutation(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                    &json!({"input":input}), crate::Collection::Agent, true,
                ).await
            })
        }).await?;
        let matched = cases
            .managed_tool_identity_cases
            .iter()
            .find(|case| case["name"] == "matched")
            .unwrap()["allowed"]
            .as_bool()
            .unwrap();
        let managed = identity::Did::new(&binding.agent_did)?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&managed, NodePermission::CollectionPatch)
                .await?,
            matched
        );
        assert!(
            node.nac_manager()
                .check_permission(&user_did, NodePermission::CollectionPatch)
                .await?
        );
        owner
            .revoke_node_permission(&user_did, NodePermission::CollectionPatch)
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&user_did, NodePermission::CollectionPatch)
                .await?,
            allowed
        );
        node.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn configured_delete_sources_grant_revoke_and_preserve_independent_permissions(
    ) -> anyhow::Result<()> {
        let (_directory, runtime, node) = projection_node().await?;
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let signer = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
        let agent = identity::Did::new(&binding.agent_did)?;
        let owner = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
        ] {
            owner.grant_node_permission(&agent, permission).await?;
        }
        node.add_collection(&format!(
            "type DeleteProjectionProbe @policy(id: \"{}\", resource: \"configuration\") {{ name: String }}",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
        )).await?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        for mutation in [
            format!(
                r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"delete-tools",self_config:{{enable_self_config:true,self_config_categories:["agent"],enable_pack_install:false}}}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"delete-context",tools_id:"delete-tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"delete-context",enabled:true}}) {{_docID}} }}"#
            ),
        ] {
            owner
                .write("test.delete_projection.create", &mutation)
                .await?;
        }
        let actor =
            crate::ConfigAccess::local_managed_agent(node.clone(), agent.clone(), signer.as_ref())?;
        let model = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration model");
        let expected = |name: &str| {
            model
                .tool_operation_cases
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled tool operation")["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| source["permission"] == NodePermission::DocumentDelete.as_str())
        };
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("self-config-agent")
        );
        let created = actor
            .write(
                "test.delete_projection.create_probe",
                r#"mutation { create_DeleteProjectionProbe(input: {name:"probe"}) {_docID} }"#,
            )
            .await?;
        let doc = crate::store::graphql::created_doc_id(&created, "DeleteProjectionProbe")?;
        let delete = format!(
            r#"mutation {{ delete_DeleteProjectionProbe(docID:"{}") {{_docID}} }}"#,
            escape_graphql_string(&doc)
        );
        let denied = actor.write("test.delete_projection.denied", &delete).await;
        assert!(!denied
            .as_ref()
            .is_ok_and(|value| value["data"]["delete_DeleteProjectionProbe"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())));
        let update = |category: &str, pack: bool| {
            format!(
                r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"delete-tools"}}}},input:{{self_config:{{enable_self_config:true,self_config_categories:["{category}"],enable_pack_install:{pack}}}}}) {{_docID}} }}"#
            )
        };
        owner
            .write(
                "test.delete_projection.enable_overlap",
                &update("tools", true),
            )
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("self-config-tools")
        );
        let deleted = actor
            .write("test.delete_projection.allowed", &delete)
            .await?;
        assert_eq!(
            deleted["data"]["delete_DeleteProjectionProbe"][0]["_docID"],
            doc
        );
        owner
            .write(
                "test.delete_projection.remove_pack",
                &update("tools", false),
            )
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("self-config-tools")
        );
        owner
            .write("test.delete_projection.pack_only", &update("agent", true))
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("pack-independent")
        );
        owner
            .write("test.delete_projection.remove_all", &update("agent", false))
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("self-config-agent")
        );
        let second = actor
            .write(
                "test.delete_projection.second_probe",
                r#"mutation { create_DeleteProjectionProbe(input: {name:"second"}) {_docID} }"#,
            )
            .await?;
        let second_doc = crate::store::graphql::created_doc_id(&second, "DeleteProjectionProbe")?;
        let second_delete = format!(
            r#"mutation {{ delete_DeleteProjectionProbe(docID:"{}") {{_docID}} }}"#,
            escape_graphql_string(&second_doc)
        );
        let denied = actor
            .write("test.delete_projection.revoked", &second_delete)
            .await;
        assert!(!denied
            .as_ref()
            .is_ok_and(|value| value["data"]["delete_DeleteProjectionProbe"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())));
        let remaining = actor
            .execute(&format!(
                r#"{{ DeleteProjectionProbe(filter: {{_docID:{{_eq:"{}"}}}}) {{_docID}} }}"#,
                escape_graphql_string(&second_doc)
            ))
            .await?;
        assert_eq!(
            remaining["data"]["DeleteProjectionProbe"][0]["_docID"],
            second_doc
        );
        owner
            .grant_node_permission(&agent, NodePermission::DocumentDelete)
            .await?;
        owner
            .write(
                "test.delete_projection.enable_again",
                &update("tools", false),
            )
            .await?;
        owner
            .write(
                "test.delete_projection.disable_again",
                &update("agent", false),
            )
            .await?;
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?
        );
        owner
            .revoke_node_permission(&agent, NodePermission::DocumentDelete)
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("self-config-agent")
        );
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
    #[tokio::test]
    async fn reviewed_plugin_delete_source_tracks_selection_and_reinstall() -> anyhow::Result<()> {
        use gents_protocol::job::PluginDataDeclaration;
        let (directory, runtime, node) = projection_node().await?;
        let outsider = crate::KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
        let outsider_did = identity::Did::new(outsider.did())?;
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let agent = identity::Did::new(&binding.agent_did)?;
        let owner = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        let digest = format!("sha256:{}", "a".repeat(64));
        let record = crate::plugin::store::InstalledPlugin {
            namespace: "team".into(),
            name: "worker".into(),
            version: "1".into(),
            digest: digest.clone(),
            language: "wasm".into(),
            declaration: crate::pack::PackPlugin {
                data: Some(PluginDataDeclaration {
                    collections: vec!["Note".into()],
                    ops: vec!["delete".into()],
                    stamped: Default::default(),
                }),
                ..Default::default()
            },
            granted: None,
            instructions: None,
            owner_pack_coordinate: None,
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        };
        let integration = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "plugins": [{"plugin":"team/worker", "digest": digest}]
        }))?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        for mutation in [
            format!(
                r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"plugin-tools",integrations:{integration}}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"plugin-context",tools_id:"plugin-tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"plugin-context",enabled:true}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{}",creation_nonce:"{nonce}",context_id:"plugin-context",enabled:true}}) {{_docID}} }}"#,
                escape_graphql_string(outsider.did())
            ),
        ] {
            owner.write("test.plugin_delete.create", &mutation).await?;
        }
        let model = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let expected = |name: &str| {
            model
                .plugin_delete_cases
                .iter()
                .find(|case| case["name"] == name)
                .unwrap()["source_count"]
                .as_u64()
                .unwrap()
                > 0
        };
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("uninstalled")
        );
        owner
            .transact_write("test.plugin_delete.install", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(&[], std::slice::from_ref(record))
                        .await
                })
            })
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("selected-installed")
        );
        assert_eq!(
            node.nac_manager()
                .check_permission(&outsider_did, NodePermission::DocumentDelete)
                .await?,
            expected("foreign-actor")
        );
        let mut replacement = record.clone();
        replacement.digest = format!("sha256:{}", "b".repeat(64));
        owner
            .transact_write("test.plugin_delete.upgrade", |txn| {
                let (old, new) = (&record, &replacement);
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(old),
                        std::slice::from_ref(new),
                    )
                    .await
                })
            })
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("replaced-digest")
        );
        owner
            .transact_write("test.plugin_delete.downgrade", |txn| {
                let (old, new) = (&replacement, &record);
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(old),
                        std::slice::from_ref(new),
                    )
                    .await
                })
            })
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("selected-installed")
        );
        owner
            .transact_write("test.plugin_delete.uninstall", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(std::slice::from_ref(record), &[])
                        .await
                })
            })
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("uninstalled")
        );
        owner.write("test.plugin_delete.disable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"plugin-tools"}}}},input:{{integrations:null}}) {{_docID}} }}"#)).await?;
        owner
            .transact_write("test.plugin_delete.reinstall", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(&[], std::slice::from_ref(record))
                        .await
                })
            })
            .await?;
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?,
            expected("disabled")
        );
        owner
            .grant_node_permission(&agent, NodePermission::DocumentDelete)
            .await?;
        owner
            .transact_write("test.plugin_delete.remove_again", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(std::slice::from_ref(record), &[])
                        .await
                })
            })
            .await?;
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?
        );
        node.shutdown().await?;
        Ok(())
    }
}
