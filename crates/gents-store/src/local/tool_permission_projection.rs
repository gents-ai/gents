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
    NodePermission::CollectionTruncate,
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

pub struct ToolPermissionProjection {
    node: identity::Did,
    runtime: Arc<dyn crate::identity::NodeIdentity>,
    policy: Arc<dyn super::ToolSourcePolicy>,
}

impl ToolPermissionProjection {
    pub fn new(
        runtime: Arc<dyn crate::identity::NodeIdentity>,
        policy: Arc<dyn super::ToolSourcePolicy>,
    ) -> std::result::Result<Self, identity::Error> {
        Ok(Self {
            node: identity::Did::new(runtime.did())?,
            runtime,
            policy,
        })
    }

    pub(super) fn node(&self) -> &identity::Did {
        &self.node
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn source_prefix(&self) -> String {
        self.prefix()
    }

    fn prefix(&self) -> String {
        source_prefix(self.node.as_str())
    }

    fn source(&self, tools: &str, context: &str, agent: &str, purpose: &str) -> String {
        tool_source(self.node.as_str(), tools, context, agent, purpose)
    }

    pub(super) async fn reconcile_installed(
        &self,
        ctx: &db::DocumentMutationContext<'_>,
        installed: &[super::ProjectedPlugin],
    ) -> Result<()> {
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            ctx.view.sibling(Namespace::Acpstore),
        ));
        let permission = NodePermission::DocumentDelete;
        let node = self.node.as_str();
        let index = super::plugin_delete_source::index(node);
        let mut wanted = BTreeSet::new();
        for plugin in installed {
            if plugin.deletes {
                wanted.insert(super::plugin_delete_source::root(
                    node,
                    &plugin.coordinate,
                    None,
                ));
                wanted.insert(super::plugin_delete_source::root(
                    node,
                    &plugin.coordinate,
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
                surfaces.push(value);
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
            let Some(agent) = field(&value, "agent_did") else {
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
                crate::identity::resolve_agent_identity(self.runtime.as_ref(), &binding)
            {
                tracing::warn!(
                    agent_did = agent,
                    error = %error,
                    "unmanaged Agent identity cannot receive tool permissions"
                );
                continue;
            }
            let baseline = self.source(&agent_doc, "", agent, "agent-baseline");
            for permission in crate::identity::GrantSet::AgentBaseline.permissions() {
                desired
                    .entry(permission.as_str())
                    .or_default()
                    .insert(baseline.clone(), agent.to_owned());
            }
            // The baseline follows the live Agent alone (Lean
            // `Identity.GrantSets.baselineWanted`); a Context is optional.
            let Some(context_id) = field(&value, "context_id") else {
                continue;
            };
            let Some((context_doc, context)) = context_by_id.get(context_id) else {
                continue;
            };
            let Some(tools_id) = self.policy.selected_tools(node, context) else {
                continue;
            };
            let Some((tools_doc, tool_value)) = tools_by_id.get(&tools_id) else {
                continue;
            };
            let Ok(operations) = self.policy.node_operation_sources(tool_value, &surfaces) else {
                continue;
            };
            if let Ok(plugins) = self.policy.plugin_refs(tool_value) {
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

fn field<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn decode_source_for_tests(object_id: &str) -> Option<String> {
    decode_source(object_id)
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

fn source_prefix(node: &str) -> String {
    let mut prefix = String::from("gents-tool-v1:");
    for byte in Sha256::digest(node.as_bytes()) {
        write!(&mut prefix, "{byte:02x}").expect("writing to a string cannot fail");
    }
    prefix.push(':');
    prefix
}

/// The source namespace relies on SHA-256 collision resistance to keep
/// independent Node, document, Agent, and purpose paths distinct.
pub fn tool_source(node: &str, tools: &str, context: &str, agent: &str, purpose: &str) -> String {
    let mut digest = Sha256::new();
    for part in [tools, context, agent, purpose] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    let mut source = source_prefix(node);
    for byte in digest.finalize() {
        write!(&mut source, "{byte:02x}").expect("writing to a string cannot fail");
    }
    source
}
