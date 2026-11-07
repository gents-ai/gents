use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use acp::{NodePermission, Subject, ZanzibarStore};
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

struct SourceChange<'a> {
    permission: NodePermission,
    parent: Option<&'a str>,
    subject_kind: &'a str,
    subject: &'a str,
    delete: bool,
}

async fn apply_source_change(
    ctx: &db::DocumentMutationContext<'_>,
    store: &acp::zanzibar::store::TransactionZanzibarStore,
    change: SourceChange<'_>,
) -> Result<()> {
    let object = change
        .parent
        .map(super::plugin_delete_source::object)
        .unwrap_or_else(|| acp::NODE_OBJECT_ID.to_owned());
    let subject = match change.subject_kind {
        "source" => Subject::entity_set(
            acp::NODE_RESOURCE_NAME,
            super::plugin_delete_source::object(change.subject),
            change.permission.as_str(),
        ),
        "actor" => Subject::Entity(identity::Did::new(change.subject).map_err(external)?),
        _ => return Err(Error::Other("invalid permission source subject".into())),
    };
    let present = store
        .get_relation_subjects(
            acp::NODE_POLICY_ID,
            acp::NODE_RESOURCE_NAME,
            &object,
            change.permission.as_str(),
        )
        .await
        .map_err(external)?
        .contains(&subject);
    if present != change.delete {
        return Ok(());
    }
    ctx.change_source_permission(
        change.permission,
        change.parent,
        change.subject_kind,
        change.subject,
        change.delete,
    )
    .await?;
    Ok(())
}

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
        ctx: &db::DocumentMutationContext<'_>,
        installed: &[crate::plugin::store::InstalledPlugin],
    ) -> Result<()> {
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            ctx.view.sibling(Namespace::Acpstore),
        ));
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
                            apply_source_change(
                                ctx,
                                &store,
                                SourceChange {
                                    permission,
                                    parent: None,
                                    subject_kind: "source",
                                    subject: &root,
                                    delete: true,
                                },
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        for root in wanted {
            apply_source_change(
                ctx,
                &store,
                SourceChange {
                    permission,
                    parent: Some(&index),
                    subject_kind: "source",
                    subject: &root,
                    delete: false,
                },
            )
            .await?;
            apply_source_change(
                ctx,
                &store,
                SourceChange {
                    permission,
                    parent: None,
                    subject_kind: "source",
                    subject: &root,
                    delete: false,
                },
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn update(&self, ctx: &db::DocumentMutationContext<'_>) -> Result<()> {
        let view = ctx.view;
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
        // Agent rows are read by field: the projection serves Agents whose rows
        // omit inference_profile_id, which the typed reference owner rejects.
        for doc in agents {
            let (agent_doc, value) = row(doc)?;
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
            let baseline = self.source(&agent_doc, "", agent, "agent-baseline");
            for permission in crate::store::identity::GrantSet::AgentBaseline.permissions() {
                desired
                    .entry(permission.as_str())
                    .or_default()
                    .insert(baseline.clone(), agent.to_owned());
            }
            let Some((context_doc, context)) = context_by_id.get(context_id) else {
                continue;
            };
            let Some(tools_id) = selected_tools(node, context) else {
                continue;
            };
            let Some((tools_doc, tool_value)) = tools_by_id.get(&tools_id) else {
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
            // Native admission reads the actor's current effective permissions,
            // so every addition is staged before any removal: a re-keyed source
            // is admitted through the path it replaces (Lean
            // `Identity.GrantSets.projectBaseline` judges the pre-mutation actor).
            for (source, agent) in &wanted {
                apply_source_change(
                    ctx,
                    &store,
                    SourceChange {
                        permission: *permission,
                        parent: Some(source),
                        subject_kind: "actor",
                        subject: agent,
                        delete: false,
                    },
                )
                .await?;
                apply_source_change(
                    ctx,
                    &store,
                    SourceChange {
                        permission: *permission,
                        parent: None,
                        subject_kind: "source",
                        subject: source,
                        delete: false,
                    },
                )
                .await?;
            }
            for source in installed.difference(&wanted.keys().cloned().collect()) {
                apply_source_change(
                    ctx,
                    &store,
                    SourceChange {
                        permission: *permission,
                        parent: None,
                        subject_kind: "source",
                        subject: source,
                        delete: true,
                    },
                )
                .await?;
            }
        }
        self.update_plugins(ctx, &store, plugin_desired).await?;
        Ok(())
    }

    async fn update_plugins(
        &self,
        ctx: &db::DocumentMutationContext<'_>,
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
            for (member, agent) in &wanted {
                apply_source_change(
                    ctx,
                    store,
                    SourceChange {
                        permission,
                        parent: Some(member),
                        subject_kind: "actor",
                        subject: agent,
                        delete: false,
                    },
                )
                .await?;
                apply_source_change(
                    ctx,
                    store,
                    SourceChange {
                        permission,
                        parent: Some(&root),
                        subject_kind: "source",
                        subject: member,
                        delete: false,
                    },
                )
                .await?;
            }
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
                            apply_source_change(
                                ctx,
                                store,
                                SourceChange {
                                    permission,
                                    parent: Some(&member),
                                    subject_kind: "actor",
                                    subject: actor.as_str(),
                                    delete: true,
                                },
                            )
                            .await?;
                        }
                    }
                    apply_source_change(
                        ctx,
                        store,
                        SourceChange {
                            permission,
                            parent: Some(&root),
                            subject_kind: "source",
                            subject: &member,
                            delete: true,
                        },
                    )
                    .await?;
                }
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

fn selected_tools(node: &str, context: &serde_json::Value) -> Option<String> {
    crate::documents::references_of(crate::Collection::AgentContext, context, node)
        .ok()?
        .into_iter()
        .find(|reference| reference.collection == crate::Collection::Tools)
        .map(|reference| reference.id)
        .filter(|id| !id.is_empty())
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

    async fn observed_plugin_input(
        owner: &crate::ConfigAccess,
        runtime: &crate::KeyIdentity,
        installed: &[crate::plugin::store::InstalledPlugin],
        actor: &identity::Did,
        direct: bool,
    ) -> anyhow::Result<serde_json::Value> {
        use serde_json::json;
        let snapshot = owner.execute(r#"{ Agent { node_did agent_did creation_nonce context_id enabled } AgentContext { _docID node_did context_id tools_id } Tools { _docID node_did tools_id integrations } }"#).await?;
        let rows = |kind: &str| -> anyhow::Result<Vec<serde_json::Value>> {
            Ok(snapshot["data"][kind]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("missing observed {kind}"))?
                .clone())
        };
        let mut agents = Vec::new();
        for agent in rows("Agent")? {
            let text = |field: &str| agent[field].as_str().unwrap_or_default().to_owned();
            let binding = gents_protocol::agent_identity::AgentIdentityBinding {
                agent_did: text("agent_did"),
                creation_nonce: text("creation_nonce"),
            };
            let derived = match crate::store::identity::resolve_agent_identity(runtime, &binding) {
                Ok(signer) => signer.did().to_owned(),
                Err(crate::store::identity::IdentityError::AgentIdentityMismatch {
                    derived_did,
                    ..
                }) => derived_did,
                Err(_) => String::new(),
            };
            agents.push(
                json!({"nodeDid":text("node_did"), "agentDid":binding.agent_did,
                "derived":derived, "contextId":text("context_id"),
                "enabled":agent["enabled"].as_bool().unwrap_or(true)}),
            );
        }
        let contexts = rows("AgentContext")?
            .into_iter()
            .map(|context| {
                json!({
                    "nodeDid":context["node_did"].as_str().unwrap_or_default(),
                    "docId":context["_docID"].as_str().unwrap_or_default(),
                    "contextId":context["context_id"].as_str().unwrap_or_default(),
                    "toolsId":context["tools_id"].as_str().unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();
        let tools = rows("Tools")?.into_iter().map(|tools| {
            let plugins = tools["integrations"]["plugins"].as_array().into_iter().flatten().map(|plugin| json!({
                "coordinate":plugin["plugin"].as_str().unwrap_or_default(), "pin":plugin["digest"].as_str(),
            })).collect::<Vec<_>>();
            json!({"nodeDid":tools["node_did"].as_str().unwrap_or_default(),
                "docId":tools["_docID"].as_str().unwrap_or_default(),
                "toolsId":tools["tools_id"].as_str().unwrap_or_default(), "plugins":plugins})
        }).collect::<Vec<_>>();
        let installed = installed.iter().map(|plugin| json!({
            "coordinate":format!("{}/{}", plugin.namespace, plugin.name), "digest":plugin.digest,
            "operations":plugin.declaration.data.as_ref().map(|data| data.ops.clone()).unwrap_or_default(),
        })).collect::<Vec<_>>();
        Ok(
            json!({"node":runtime.did(), "installed":installed, "agents":agents,
            "contexts":contexts, "tools":tools, "actor":actor.as_str(), "direct":direct}),
        )
    }

    async fn observe_plugin_permission(
        observations: &mut Vec<(serde_json::Value, bool)>,
        owner: &crate::ConfigAccess,
        runtime: &crate::KeyIdentity,
        node: &defra_node::Node,
        installed: &[crate::plugin::store::InstalledPlugin],
        actor: &identity::Did,
        direct: bool,
    ) -> anyhow::Result<()> {
        let input = observed_plugin_input(owner, runtime, installed, actor, direct).await?;
        let observed = node
            .nac_manager()
            .check_permission(actor, NodePermission::DocumentDelete)
            .await?;
        observations.push((input, observed));
        Ok(())
    }

    async fn write_fixture_tools(
        access: &crate::ConfigAccess,
        node_did: &str,
        agent_did: &str,
        tools: serde_json::Value,
    ) -> anyhow::Result<()> {
        use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let mut value = serde_json::json!({"node_did": node_did, "tools_id": format!("{agent_did}:tools")});
        for (key, field) in tools.as_object().expect("Tools fields") {
            value[key] = field.clone();
        }
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: crate::Collection::Tools,
            add: value.clone(),
            update: value,
        }])?;
        access
            .transact_write("test.projection.fixture_tools", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::store::config_client::apply_desired_state_plan(txn, plan).await
                })
            })
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn enrolled_user_authors_held_tools_and_only_the_household_owner_adds_schema(
    ) -> anyhow::Result<()> {
        use crate::store::identity::GrantSet;
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_delegation_cases;
        let modeled = |name: &str| {
            cases
                .iter()
                .find(|case| case.name == name)
                .unwrap_or_else(|| panic!("modeled delegation {name:?}"))
                .expected
        };
        let (db, _user, user_access, binding) =
            crate::test_support::signed_test_node("enrolled-tools-authoring").await;
        let node_did = db.node_identity.did().to_owned();
        let agent = identity::Did::new(&binding.agent_did)?;
        let self_config =
            serde_json::to_value(crate::tools::builtin::self_config::engineer_self_config())?;
        let held = write_fixture_tools(
            &user_access,
            &node_did,
            &binding.agent_did,
            serde_json::json!({"self_config": self_config}),
        )
        .await;
        assert_eq!(held.is_ok(), modeled("enrolled-grants-held"), "{held:?}");
        assert!(
            db.node
                .nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?
        );
        let schema = serde_json::json!({
            "self_config": self_config,
            "built_ins": {"enable_schema_tool": true},
        });
        let refused =
            write_fixture_tools(&user_access, &node_did, &binding.agent_did, schema.clone()).await;
        assert_eq!(
            refused.is_ok(),
            modeled("enrolled-refused-schema"),
            "{refused:?}"
        );
        assert!(
            !db.node
                .nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );

        let householder = tokio::task::spawn_blocking(|| {
            crate::support::fixtures::test_identity("enrolled-tools-householder")
        })
        .await?;
        let householder_did = identity::Did::new(householder.did())?;
        let runtime =
            crate::ConfigAccess::local_as(db.node.clone(), identity::Did::new(&node_did)?);
        GrantSet::HouseholdOwner
            .grant(&runtime, &householder_did)
            .await?;
        let node_doc = user_access.execute("{ Node { _docID } }").await?["data"]["Node"][0]
            ["_docID"]
            .as_str()
            .expect("fixture Node")
            .to_owned();
        user_access
            .add_actor_relationship("Node", &node_doc, "administrator", &householder_did)
            .await?;
        let owner_write = write_fixture_tools(
            &db.client_access(&householder),
            &node_did,
            &binding.agent_did,
            schema,
        )
        .await;
        assert_eq!(
            owner_write.is_ok(),
            modeled("owner-grants-schema"),
            "{owner_write:?}"
        );
        assert!(
            db.node
                .nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        db.node.shutdown().await?;
        Ok(())
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
    async fn http_non_admin_tools_writer_cannot_grant_managed_agent_schema_authority(
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
        let agent_did = identity::Did::new(&binding.agent_did)?;
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
        ] {
            owner.grant_node_permission(&user_did, permission).await?;
        }
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let response = owner.write("test.http_projection.tools", &format!(
            r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"http-tools",display_name:"before",built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#
        )).await?;
        let tools_doc = crate::store::graphql::created_doc_id(&response, "Tools")?;
        for mutation in [
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"http-context",tools_id:"http-tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"http-context",enabled:true}}) {{_docID}} }}"#
            ),
        ] {
            owner.write("test.http_projection.setup", &mutation).await?;
        }
        owner
            .add_actor_relationship("Tools", &tools_doc, "writer", &user_did)
            .await?;
        assert!(!node.nac_manager().is_admin(&user_did).await?);
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent_did, NodePermission::CollectionPatch)
                .await?
        );
        let model = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration model");
        let model_non_admin = model
            ._tool_grantor_cases
            .iter()
            .find(|case| {
                case["committed"] == true
                    && case["admins"]
                        .as_array()
                        .is_some_and(|admins| !admins.contains(&case["grantor"]))
            })
            .expect("non-admin writer model");
        assert!(model_non_admin["expected_grants"]
            .as_array()
            .expect("modeled grants")
            .is_empty());

        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        let mutation = format!(
            r#"mutation($input: ToolsMutationInputArg!) {{ update_Tools(docID:"{}", input:$input) {{_docID}} }}"#,
            escape_graphql_string(&tools_doc)
        );
        let harmless = json!({"input":{"display_name":"reviewed"}});
        let response = access
            .transact_write("test.http_projection.harmless", |txn| {
                let (mutation, harmless) = (&mutation, &harmless);
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        mutation,
                        harmless,
                        crate::Collection::Tools,
                        false,
                    )
                    .await
                })
            })
            .await?;
        assert_eq!(response["data"]["update_Tools"][0]["_docID"], tools_doc);
        let elevated = json!({"input":{"built_ins":{"enable_schema_tool":true}}});
        let failure = access
            .transact_write("test.http_projection.denied", |txn| {
                let (mutation, elevated) = (&mutation, &elevated);
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        mutation,
                        elevated,
                        crate::Collection::Tools,
                        false,
                    )
                    .await
                })
            })
            .await
            .expect_err("a non-admin cannot create the required native source grant");
        assert!(failure.is_permission_denied(), "{failure:?}");
        assert!(!failure.is_retryable_database_conflict());
        let persisted = owner.execute(&format!(r#"{{ Tools(filter: {{_docID: {{_eq: "{}"}}}}) {{ _docID display_name built_ins }} }}"#, escape_graphql_string(&tools_doc))).await?;
        assert_eq!(persisted["data"]["Tools"][0]["_docID"], tools_doc);
        assert_eq!(persisted["data"]["Tools"][0]["display_name"], "reviewed");
        assert_eq!(
            persisted["data"]["Tools"][0]["built_ins"]["enable_schema_tool"],
            false
        );
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent_did, NodePermission::CollectionPatch)
                .await?
        );
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
        ] {
            access.transact_write("test.user_signed_tool_configuration", |txn| {
                let input = &input;
                Box::pin(async move { txn.execute_desired_state_mutation(mutation, &json!({"input":input}), collection, true).await })
            }).await?;
        }
        use crate::lean_vocab_test::LeanClaimedDid;
        let identity_cases = &crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap()
            .managed_tool_identity_cases;
        assert!(identity_cases.iter().any(|case| case.allowed));
        assert!(identity_cases.iter().any(|case| !case.allowed));
        let key = |name: &str| match name {
            "runtime" => crate::KeyIdentity::load_or_create(&path, None),
            other => crate::KeyIdentity::load_or_create(directory.path().join(other), None),
        };
        let mut rows = Vec::new();
        for case in identity_cases {
            anyhow::ensure!(case.node_key == "runtime", "projecting Node key: {case:?}");
            let claimed = match &case.claimed {
                None => String::new(),
                Some(LeanClaimedDid::Key(name)) => key(name)?.did().to_owned(),
                Some(LeanClaimedDid::Derived {
                    node_key,
                    creation_nonce,
                }) => crate::store::identity::derive_agent_identity(&key(node_key)?, creation_nonce)?
                    .did()
                    .to_owned(),
            };
            let derived = case.creation_nonce.as_deref().and_then(|nonce| {
                crate::store::identity::derive_agent_identity(&runtime, nonce)
                    .ok()
                    .map(|identity| identity.did().to_owned())
            });
            let mut input = json!({"node_did":runtime.did(),"agent_did":claimed,
                "context_id":"context","enabled":true});
            if let Some(nonce) = &case.creation_nonce {
                input["creation_nonce"] = json!(nonce);
            }
            let subjects = std::iter::once(claimed)
                .chain(derived)
                .filter(|did| !did.is_empty())
                .map(identity::Did::new)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.push((case, input, subjects));
        }
        let projected = |subjects: &[identity::Did]| {
            let node = node.clone();
            let subjects = subjects.to_vec();
            async move {
                let mut granted = Vec::new();
                for subject in &subjects {
                    granted.push(
                        node.nac_manager()
                            .check_permission(subject, NodePermission::CollectionPatch)
                            .await?,
                    );
                }
                anyhow::Ok(granted)
            }
        };
        for (case, input, subjects) in rows.iter().filter(|(case, ..)| !case.allowed) {
            access.transact_write("test.user_signed_unmanaged_agent", |txn| {
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                        &json!({"input":input}), crate::Collection::Agent, true,
                    ).await
                })
            }).await?;
            for granted in projected(subjects).await? {
                assert_eq!(granted, case.allowed, "{case:?}");
            }
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
        for (case, input, subjects) in rows.iter().filter(|(case, ..)| case.allowed) {
            let denied = access.transact_write("test.user_signed_managed_agent", |txn| {
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                        &json!({"input":input}), crate::Collection::Agent, true,
                    ).await
                })
            }).await.expect_err("a non-admin cannot revoke the legacy source or grant the managed Agent");
            assert!(denied.is_permission_denied(), "{denied:?}");
            assert!(projected(subjects).await?.iter().all(|granted| !granted));
            let stored = owner
                .execute(&format!(
                    r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                    escape_graphql_string(input["agent_did"].as_str().unwrap_or_default())
                ))
                .await?;
            assert!(
                stored["data"]["Agent"].as_array().is_some_and(Vec::is_empty),
                "{stored}"
            );
            owner.transact_write("test.owner_managed_agent", |txn| {
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                        &json!({"input":input}), crate::Collection::Agent, true,
                    ).await
                })
            }).await?;
            for granted in projected(subjects).await? {
                assert_eq!(granted, case.allowed, "{case:?}");
            }
        }
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
            rows.iter()
                .any(|(case, _, subjects)| case.allowed && subjects.contains(&user_did))
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
    async fn non_admin_tools_writer_cannot_project_schema_authority() -> anyhow::Result<()> {
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
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let response = owner.write("test.projection.writer_tools", &format!(
            r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"writer-tools",built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#
        )).await?;
        let tools_doc = crate::store::graphql::created_doc_id(&response, "Tools")?;
        for mutation in [
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"writer-context",tools_id:"writer-tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"writer-context",enabled:true}}) {{_docID}} }}"#
            ),
        ] {
            owner
                .write("test.projection.writer_setup", &mutation)
                .await?;
        }
        owner
            .add_actor_relationship("Tools", &tools_doc, "writer", &agent)
            .await?;
        assert!(!node.nac_manager().is_admin(&agent).await?);
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );

        let model = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration model");
        let case = model
            ._tool_grantor_cases
            .iter()
            .find(|case| {
                case["committed"] == true
                    && case["admins"]
                        .as_array()
                        .is_some_and(|admins| !admins.contains(&case["grantor"]))
            })
            .expect("non-admin Tools writer model");
        let expected_grants = case["expected_grants"].as_array().expect("modeled grants");
        assert!(expected_grants.is_empty());

        let writer =
            crate::ConfigAccess::local_managed_agent(node.clone(), agent.clone(), signer.as_ref())?;
        let mutation = format!(
            r#"mutation($input: ToolsMutationInputArg!) {{ update_Tools(docID:"{}", input:$input) {{_docID}} }}"#,
            escape_graphql_string(&tools_doc)
        );
        let harmless = serde_json::json!({"input":{"built_ins":{"enable_schema_tool":false}}});
        let response = writer
            .transact_write("test.projection.writer_control", |txn| {
                let mutation = &mutation;
                let harmless = &harmless;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        harmless,
                        gents_protocol::schemas::TOOLS,
                    )
                    .await
                })
            })
            .await?;
        assert_eq!(response["data"]["update_Tools"][0]["_docID"], tools_doc);

        let elevated = serde_json::json!({"input":{"built_ins":{"enable_schema_tool":true}}});
        let attempt = writer
            .transact_write("test.projection.writer_schema", |txn| {
                let mutation = &mutation;
                let elevated = &elevated;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        elevated,
                        gents_protocol::schemas::TOOLS,
                    )
                    .await
                })
            })
            .await;
        assert!(
            attempt.is_err(),
            "an unauthorized required grant must abort its Tools update"
        );
        let persisted = owner
            .execute(&format!(
                r#"{{ Tools(filter: {{_docID: {{_eq: "{}"}}}}) {{ _docID built_ins }} }}"#,
                escape_graphql_string(&tools_doc)
            ))
            .await?;
        assert_eq!(persisted["data"]["Tools"][0]["_docID"], tools_doc);
        assert_eq!(
            persisted["data"]["Tools"][0]["built_ins"]["enable_schema_tool"],
            false
        );
        assert_eq!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?,
            !expected_grants.is_empty()
        );

        let expected_change = |name: &str| {
            model
                .tool_source_change_cases
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled source change")["allowed"]
                .as_bool()
                .expect("modeled decision")
        };
        owner.write("test.projection.owner_schema", &format!(
            r#"mutation {{ update_Tools(docID:"{}", input:{{built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#,
            escape_graphql_string(&tools_doc)
        )).await?;
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        let unchanged = writer
            .transact_write("test.projection.writer_noop", |txn| {
                let mutation = &mutation;
                let elevated = &elevated;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        elevated,
                        gents_protocol::schemas::TOOLS,
                    )
                    .await
                })
            })
            .await;
        assert_eq!(unchanged.is_ok(), expected_change("present-noop"));
        let disable = writer
            .transact_write("test.projection.writer_disable", |txn| {
                let mutation = &mutation;
                let harmless = &harmless;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        harmless,
                        gents_protocol::schemas::TOOLS,
                    )
                    .await
                })
            })
            .await;
        assert_eq!(disable.is_ok(), expected_change("delete-denied"));
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        owner.write("test.projection.owner_disable", &format!(
            r#"mutation {{ update_Tools(docID:"{}", input:{{built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#,
            escape_graphql_string(&tools_doc)
        )).await?;
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        let dormant = writer
            .transact_write("test.projection.writer_dormant", |txn| {
                let mutation = &mutation;
                let elevated = &elevated;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        elevated,
                        gents_protocol::schemas::TOOLS,
                    )
                    .await
                })
            })
            .await;
        assert_eq!(dormant.is_ok(), expected_change("add-denied"));
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, NodePermission::CollectionPatch)
                .await?
        );
        owner.write("test.projection.owner_restore", &format!(
            r#"mutation {{ update_Tools(docID:"{}", input:{{built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#,
            escape_graphql_string(&tools_doc)
        )).await?;
        node.database().reconcile_document_projector().await?;
        let txn = node.database().new_txn(false).await?;
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            txn.datastore()?.sibling(Namespace::Acpstore),
        ));
        let roots = store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                acp::NODE_OBJECT_ID,
                NodePermission::CollectionPatch.as_str(),
            )
            .await?;
        let source = roots
            .into_iter()
            .filter_map(|subject| match subject {
                Subject::EntitySet {
                    resource,
                    object_id,
                    relation,
                } if resource == acp::NODE_RESOURCE_NAME
                    && relation == NodePermission::CollectionPatch.as_str() =>
                {
                    decode_source(&object_id).filter(|source| source.starts_with("gents-tool-v1:"))
                }
                _ => None,
            })
            .next()
            .expect("schema source root");
        let nac = acp::NodeACP::new(store.clone());
        nac.load().await?;
        nac.change_source_permission(
            &identity::Did::new(runtime.did())?,
            NodePermission::CollectionPatch,
            None,
            "source",
            &source,
            true,
        )
        .await?;
        drop(nac);
        drop(store);
        txn.commit().await?;
        assert!(
            node.database()
                .reconcile_document_projector()
                .await
                .is_err(),
            "actorless reconciliation must not restore a missing source"
        );
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
        let mut observations = Vec::new();
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            &[],
            &agent,
            false,
        )
        .await?;
        owner
            .transact_write("test.plugin_delete.install", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(&[], std::slice::from_ref(record))
                        .await
                })
            })
            .await?;
        let signer = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
        let non_admin =
            crate::ConfigAccess::local_managed_agent(node.clone(), agent.clone(), signer.as_ref())?;
        assert!(!node.nac_manager().is_admin(&agent).await?);
        let modeled = &crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration model")
            .tool_source_change_cases;
        let expected_change = |name: &str| {
            modeled
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled source change")["allowed"]
                .as_bool()
                .expect("modeled decision")
        };
        let repeat = non_admin
            .transact_write("test.plugin_delete.same", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(record),
                        std::slice::from_ref(record),
                    )
                    .await
                })
            })
            .await;
        assert_eq!(
            repeat.is_ok(),
            expected_change("present-noop"),
            "{repeat:?}"
        );

        let txn = node.database().new_txn(false).await?;
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            txn.datastore()?.sibling(Namespace::Acpstore),
        ));
        let nac = acp::NodeACP::new(store.clone());
        nac.load().await?;
        let index = super::super::plugin_delete_source::index(runtime.did());
        let root = super::super::plugin_delete_source::root(runtime.did(), "team/worker", None);
        nac.change_source_permission(
            &identity::Did::new(runtime.did())?,
            NodePermission::DocumentDelete,
            Some(&index),
            "source",
            &root,
            true,
        )
        .await?;
        drop(nac);
        drop(store);
        txn.commit().await?;
        let missing = non_admin
            .transact_write("test.plugin_delete.missing", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(record),
                        std::slice::from_ref(record),
                    )
                    .await
                })
            })
            .await;
        assert_eq!(
            missing.is_ok(),
            expected_change("add-denied"),
            "{missing:?}"
        );
        owner
            .transact_write("test.plugin_delete.repair", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(record),
                        std::slice::from_ref(record),
                    )
                    .await
                })
            })
            .await?;
        let txn = node.database().new_txn(false).await?;
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            txn.datastore()?.sibling(Namespace::Acpstore),
        ));
        let pinned_root = super::super::plugin_delete_source::root(
            runtime.did(),
            "team/worker",
            Some(&record.digest),
        );
        let children = store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                &super::super::plugin_delete_source::object(&pinned_root),
                NodePermission::DocumentDelete.as_str(),
            )
            .await?;
        let member = children
            .into_iter()
            .filter_map(|subject| match subject {
                Subject::EntitySet {
                    resource,
                    object_id,
                    relation,
                } if resource == acp::NODE_RESOURCE_NAME
                    && relation == NodePermission::DocumentDelete.as_str() =>
                {
                    decode_source(&object_id).filter(|source| {
                        source.starts_with(&super::super::plugin_delete_source::member_prefix(
                            runtime.did(),
                        ))
                    })
                }
                _ => None,
            })
            .next()
            .expect("selected plugin membership");
        let nac = acp::NodeACP::new(store.clone());
        nac.load().await?;
        nac.change_source_permission(
            &identity::Did::new(runtime.did())?,
            NodePermission::DocumentDelete,
            Some(&member),
            "actor",
            agent.as_str(),
            true,
        )
        .await?;
        drop(nac);
        drop(store);
        txn.commit().await?;
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?
        );
        let missing_member = non_admin
            .transact_write("test.plugin_delete.missing_member", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(record),
                        std::slice::from_ref(record),
                    )
                    .await
                })
            })
            .await;
        assert_eq!(
            missing_member.is_ok(),
            expected_change("add-denied"),
            "{missing_member:?}"
        );
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?
        );
        owner
            .transact_write("test.plugin_delete.repair_member", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        std::slice::from_ref(record),
                        std::slice::from_ref(record),
                    )
                    .await
                })
            })
            .await?;
        assert!(
            node.nac_manager()
                .check_permission(&agent, NodePermission::DocumentDelete)
                .await?
        );
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            std::slice::from_ref(&record),
            &agent,
            false,
        )
        .await?;
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            std::slice::from_ref(&record),
            &outsider_did,
            false,
        )
        .await?;
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
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            std::slice::from_ref(&replacement),
            &agent,
            false,
        )
        .await?;
        let unpinned = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "plugins": [{"plugin":"team/worker"}]
        }))?;
        owner.write("test.plugin_delete.unpinned", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"plugin-tools"}}}},input:{{integrations:{unpinned}}}) {{_docID}} }}"#)).await?;
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            std::slice::from_ref(&replacement),
            &agent,
            false,
        )
        .await?;
        owner.write("test.plugin_delete.restore_pin", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"plugin-tools"}}}},input:{{integrations:{integration}}}) {{_docID}} }}"#)).await?;
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
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            std::slice::from_ref(&record),
            &agent,
            false,
        )
        .await?;
        owner
            .transact_write("test.plugin_delete.uninstall", |txn| {
                let record = &record;
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(std::slice::from_ref(record), &[])
                        .await
                })
            })
            .await?;
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            &[],
            &agent,
            false,
        )
        .await?;
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
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            std::slice::from_ref(&record),
            &agent,
            false,
        )
        .await?;
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
        observe_plugin_permission(
            &mut observations,
            &owner,
            &runtime,
            &node,
            &[],
            &agent,
            true,
        )
        .await?;
        let inputs = observations
            .iter()
            .map(|(input, _)| input.clone())
            .collect::<Vec<_>>();
        let serialized = serde_json::to_string(&inputs)?;
        let output = tokio::task::spawn_blocking(move || {
            gents_lean_contract::run_prepared_generator_with_input(
                &gents_lean_contract::proofs_dir()?,
                "Proofs.Conformance.PluginDelete:olean",
                "Proofs/Conformance/PluginDelete.lean",
                &serialized,
            )
        })
        .await??;
        let modeled: Vec<bool> = serde_json::from_str(&output)?;
        assert_eq!(modeled.len(), observations.len());
        for (expected, (input, actual)) in modeled.into_iter().zip(observations) {
            assert_eq!(actual, expected, "observed fixture: {input}");
        }
        node.shutdown().await?;
        Ok(())
    }
}
