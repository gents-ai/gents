use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use gents_documents::{
    references_of, AgentTargetDocument, DatastoreToolSurfaceDocument, EventSource, PageDocument,
    PluginSourceDocument, Tools, PLUGIN_SOURCE_NAMESPACE,
};
use gents_store::collection::Collection;

/// What one document kind becomes in a pack.
pub(super) enum PackPart {
    /// Rows of this `pack_config.json` list.
    Documents(&'static str),
}

/// The one place a document kind is made exportable: `None` is a kind a pack
/// cannot carry, so a reference to it refuses the export. A new kind (a page,
/// an inline-source plugin) joins export by returning its part here.
pub(super) fn pack_part(collection: Collection) -> Option<PackPart> {
    match collection {
        Collection::Agent
        | Collection::AgentContext
        | Collection::Compaction
        | Collection::Skill
        | Collection::DatastoreToolSurface
        | Collection::Tools
        | Collection::AgentTarget
        | Collection::Task
        | Collection::Schedule
        | Collection::EventSource
        | Collection::Trigger
        | Collection::Page
        | Collection::PluginSource => collection.dir_name().map(PackPart::Documents),
        Collection::Node
        | Collection::DefaultAgentSelection
        | Collection::InferenceBackend
        | Collection::InferenceProfile
        | Collection::InferenceSampling
        | Collection::InferenceExecution
        | Collection::InferenceRetryPolicy
        | Collection::ToolServiceRegistry
        | Collection::ProjectionAcpBinding
        | Collection::Callback
        | Collection::CallbackBinding
        | Collection::CallbackModule
        | Collection::RepositoryPlacement
        | Collection::EvalDefinition => None,
    }
}

/// The selection words `pack export` takes, each naming one exportable kind.
pub const SELECTABLE_KINDS: &[(&str, Collection)] = &[
    ("agent", Collection::Agent),
    ("context", Collection::AgentContext),
    ("tools", Collection::Tools),
    ("skill", Collection::Skill),
    ("compaction", Collection::Compaction),
    ("datastore", Collection::DatastoreToolSurface),
    ("agent-target", Collection::AgentTarget),
    ("task", Collection::Task),
    ("trigger", Collection::Trigger),
    ("event-source", Collection::EventSource),
    ("schedule", Collection::Schedule),
    ("page", Collection::Page),
    ("plugin-source", Collection::PluginSource),
];

pub fn selectable_kind(word: &str) -> Option<Collection> {
    SELECTABLE_KINDS
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, collection)| *collection)
}

/// The documents to export: explicit logical IDs per kind, application
/// collections by name, and every document carrying one of `tags`.
#[derive(Debug, Clone, Default)]
pub struct ExportSelection {
    pub documents: BTreeSet<(Collection, String)>,
    pub collections: BTreeSet<String>,
    pub tags: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PulledByReference {
    pub part: String,
    pub via: String,
}

pub(super) struct Closure {
    pub documents: BTreeMap<(Collection, String), Value>,
    pub selected: Vec<String>,
    pub pulled: Vec<PulledByReference>,
    /// Application collections whose schema the pack ships.
    pub collections: BTreeSet<String>,
    /// The plugin sources an exported Tools selects as `node/NAME`: the pack
    /// ships each as its own plugin, built from that source.
    pub plugins: BTreeSet<String>,
}

/// The plugin source a `node/NAME` selection was built from.
pub(super) fn built_here(plugin: &str) -> Option<&str> {
    plugin
        .strip_prefix(PLUGIN_SOURCE_NAMESPACE)?
        .strip_prefix('/')
}

fn label(collection: Collection, id: &str) -> String {
    format!("{}/{id}", collection.graphql_type())
}

fn tagged(document: &Value, tags: &BTreeSet<String>) -> bool {
    document["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|tag| tags.contains(tag))
}

/// The selection and everything it references, or every reason it cannot be
/// a pack: a selected ID that is not on the node, and each reference that
/// leaves the selection for something a pack cannot carry.
pub(super) fn close(
    node_did: &str,
    snapshot: &BTreeMap<Collection, Vec<Value>>,
    selection: &ExportSelection,
) -> Result<Closure> {
    let live: BTreeMap<(Collection, String), &Value> = snapshot
        .iter()
        .flat_map(|(collection, rows)| {
            rows.iter().filter_map(|row| {
                Some((
                    (
                        *collection,
                        row[collection.unique_field()].as_str()?.to_owned(),
                    ),
                    row,
                ))
            })
        })
        .collect();
    let mut refusals = Vec::new();
    let mut queue: Vec<(Collection, String)> = Vec::new();
    for (collection, id) in &selection.documents {
        if pack_part(*collection).is_none() {
            refusals.push(format!(
                "{} is not a kind a pack carries",
                label(*collection, id)
            ));
        } else if live.contains_key(&(*collection, id.clone())) {
            queue.push((*collection, id.clone()));
        } else {
            refusals.push(format!(
                "{} is not on this node, or not readable by the exporting actor",
                label(*collection, id)
            ));
        }
    }
    if !selection.tags.is_empty() {
        let before = queue.len();
        queue.extend(
            live.iter()
                .filter(|(_, document)| tagged(document, &selection.tags))
                .map(|(key, _)| key.clone()),
        );
        if queue.len() == before {
            refusals.push(format!(
                "no document carries the tag {}",
                selection
                    .tags
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" or ")
            ));
        }
    }
    let selected: BTreeSet<(Collection, String)> = queue.iter().cloned().collect();
    let mut documents = BTreeMap::new();
    let mut pulled = Vec::new();
    let managed = |name: &str| {
        gents_store::schema::migration::DEFAULT_REGISTRY
            .managed_names()
            .any(|managed| managed == name)
    };
    let mut collections = BTreeSet::new();
    for name in &selection.collections {
        if managed(name) {
            refusals.push(format!(
                "collection {name} is managed by Gents; a pack carries application collections only"
            ));
        } else {
            collections.insert(name.clone());
        }
    }
    let mut plugins = BTreeSet::new();
    let mut pull_collection = |name: &str, via: String, pulled: &mut Vec<PulledByReference>| {
        if !managed(name) && collections.insert(name.to_owned()) {
            pulled.push(PulledByReference {
                part: format!("collection/{name}"),
                via,
            });
        }
    };
    while let Some((collection, id)) = queue.pop() {
        let key = (collection, id.clone());
        if documents.contains_key(&key) {
            continue;
        }
        let document = live[&key];
        documents.insert(key, document.clone());
        let source = label(collection, &id);
        for reference in references_of(collection, document, node_did)? {
            if collection == Collection::Agent
                && reference.collection == Collection::InferenceProfile
            {
                continue;
            }
            let target = label(reference.collection, &reference.id);
            if pack_part(reference.collection).is_none() {
                refusals.push(format!(
                    "{source} {} references {target}, which a pack cannot carry",
                    reference.field
                ));
                continue;
            }
            let target_key = (reference.collection, reference.id.clone());
            if !live.contains_key(&target_key) {
                refusals.push(format!(
                    "{source} {} references {target}, which is not on this node or not readable by the exporting actor",
                    reference.field
                ));
                continue;
            }
            if !selected.contains(&target_key)
                && !documents.contains_key(&target_key)
                && !queue.contains(&target_key)
            {
                pulled.push(PulledByReference {
                    part: target,
                    via: format!("{source} {}", reference.field),
                });
            }
            queue.push(target_key);
        }
        match collection {
            Collection::DatastoreToolSurface => {
                let surface: DatastoreToolSurfaceDocument =
                    serde_json::from_value(document.clone())?;
                for entry in surface.entries.iter().flatten() {
                    pull_collection(
                        entry.collection(),
                        format!("{source} tool {}", entry.tool_name()),
                        &mut pulled,
                    );
                }
            }
            Collection::EventSource => {
                let event_source: EventSource = serde_json::from_value(document.clone())?;
                pull_collection(
                    &event_source.source_collection,
                    format!("{source} source_collection"),
                    &mut pulled,
                );
            }
            Collection::Page => {
                let page: PageDocument = serde_json::from_value(document.clone())?;
                for granted in page.data.iter().flatten() {
                    let target_key = (Collection::DatastoreToolSurface, granted.surface.clone());
                    let target = label(target_key.0, &target_key.1);
                    if !live.contains_key(&target_key) {
                        refusals.push(format!(
                            "{source} data grants {target}, which is not on this node or not readable by the exporting actor"
                        ));
                        continue;
                    }
                    if !selected.contains(&target_key)
                        && !documents.contains_key(&target_key)
                        && !queue.contains(&target_key)
                    {
                        pulled.push(PulledByReference {
                            part: target,
                            via: format!("{source} data"),
                        });
                    }
                    queue.push(target_key);
                }
            }
            Collection::PluginSource => {
                let plugin: PluginSourceDocument = serde_json::from_value(document.clone())?;
                for name in plugin.data.iter().flat_map(|data| &data.collections) {
                    pull_collection(name, format!("{source} data.collections"), &mut pulled);
                }
            }
            Collection::AgentTarget => {
                let target: AgentTargetDocument = serde_json::from_value(document.clone())?;
                if target.target_node_did != node_did {
                    refusals.push(format!(
                        "{source} names agent {} on another node {}, which a pack cannot carry",
                        target.agent_did, target.target_node_did
                    ));
                }
            }
            Collection::Tools => {
                if let Some(root) = document["host"]["root"].as_str() {
                    refusals.push(format!(
                        "{source} host.root is {root}, a path on this node, which a pack cannot carry; clear the agent's host tools or leave it out"
                    ));
                }
                let tools: Tools = serde_json::from_value(document.clone())?;
                for plugin in tools
                    .integrations
                    .as_ref()
                    .and_then(|integrations| integrations.plugins.as_deref())
                    .unwrap_or_default()
                {
                    let Some(name) = built_here(&plugin.plugin) else {
                        refusals.push(format!(
                            "{source} selects plugin {}, which an installed pack ships; pack export carries only the node/NAME plugins built from a PluginSource on this node",
                            plugin.plugin
                        ));
                        continue;
                    };
                    let target_key = (Collection::PluginSource, name.to_owned());
                    let target = label(target_key.0, &target_key.1);
                    if !live.contains_key(&target_key) {
                        refusals.push(format!(
                            "{source} selects plugin {}, and {target}, the source a pack would build it from, is not on this node or not readable by the exporting actor",
                            plugin.plugin
                        ));
                        continue;
                    }
                    if !selected.contains(&target_key)
                        && !documents.contains_key(&target_key)
                        && !queue.contains(&target_key)
                    {
                        pulled.push(PulledByReference {
                            part: target,
                            via: format!("{source} integrations.plugins"),
                        });
                    }
                    plugins.insert(name.to_owned());
                    queue.push(target_key);
                }
            }
            _ => {}
        }
    }
    anyhow::ensure!(
        refusals.is_empty(),
        "this selection cannot be exported as a pack:\n- {}",
        refusals.join("\n- ")
    );
    anyhow::ensure!(
        !documents.is_empty() || !collections.is_empty(),
        "pack export needs a selection: IDs per kind ({}), collection names, or a tag",
        SELECTABLE_KINDS
            .iter()
            .map(|(word, _)| *word)
            .collect::<Vec<_>>()
            .join(", ")
    );
    pulled.sort_by(|left, right| left.part.cmp(&right.part));
    Ok(Closure {
        documents,
        selected: selected
            .iter()
            .map(|(collection, id)| label(*collection, id))
            .chain(
                selection
                    .collections
                    .iter()
                    .map(|name| format!("collection/{name}")),
            )
            .collect(),
        pulled,
        collections,
        plugins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gents_managed_collection_named_for_export_is_refused_by_name() {
        let selection = ExportSelection {
            collections: ["Agent".to_owned(), "Notes".to_owned()].into(),
            ..Default::default()
        };
        let Err(error) = close("did:key:n", &BTreeMap::new(), &selection) else {
            panic!("a managed collection was accepted");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("collection Agent is managed by Gents"),
            "{message}"
        );
        assert!(!message.contains("Notes"), "{message}");
    }
}
