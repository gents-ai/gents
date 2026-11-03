//! JSON Schemas of what a user authors: a pack's `pack_config.json` and its
//! `manifest.json`, and each canonical configuration document they hold.

use gents::documents::{
    Agent, AgentContext, AgentTargetDocument, Callback, CallbackBinding, CallbackModule,
    CompactionConfig, DatastoreToolSurfaceDocument, EvalDefinition, EventSource, InferenceBackend,
    InferenceExecution, InferenceProfile, InferenceRetryPolicy, InferenceSampling, Node,
    PackConfig, ProjectionAcpBinding, RepositoryPlacement, Schedule, SkillDocument, Task,
    ToolServiceRegistry, Tools, Trigger,
};
use gents::pack::PackManifest;
use gents::Collection;
use schemars::{schema_for, Schema};
use serde::Serialize;
use serde_json::Value;

pub(crate) struct ConfigDocument {
    pub(crate) id: String,
    pub(crate) schema: Schema,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConfigIndexEntry {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) schema: String,
}

impl ConfigDocument {
    fn new(id: impl Into<String>, schema: Schema) -> Self {
        Self {
            id: id.into(),
            schema,
        }
    }

    pub(crate) fn index_entry(&self) -> ConfigIndexEntry {
        let text = |key: &str| {
            self.schema
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        ConfigIndexEntry {
            title: text("title"),
            description: text("description"),
            schema: format!("config/{}.schema.json", self.id),
            id: self.id.clone(),
        }
    }
}

/// The pack root first, then every canonical collection in apply order, then
/// the pack manifest.
pub(crate) fn config_reference() -> Vec<ConfigDocument> {
    let mut documents = Vec::with_capacity(Collection::ALL.len() + 2);
    documents.push(ConfigDocument::new("pack_config", schema_for!(PackConfig)));
    documents.extend(Collection::ALL.into_iter().map(|collection| {
        ConfigDocument::new(document_id(collection), document_schema(collection))
    }));
    documents.push(ConfigDocument::new(
        "pack_manifest",
        schema_for!(PackManifest),
    ));
    documents
}

/// The collection's GraphQL type name in snake_case.
pub(crate) fn document_id(collection: Collection) -> String {
    let mut id = String::new();
    for (index, character) in collection.graphql_type().char_indices() {
        if character.is_ascii_uppercase() && index > 0 {
            id.push('_');
        }
        id.push(character.to_ascii_lowercase());
    }
    id
}

fn document_schema(collection: Collection) -> Schema {
    match collection {
        Collection::Node => schema_for!(Node),
        Collection::Agent => schema_for!(Agent),
        Collection::AgentContext => schema_for!(AgentContext),
        Collection::Compaction => schema_for!(CompactionConfig),
        Collection::Skill => schema_for!(SkillDocument),
        Collection::DatastoreToolSurface => schema_for!(DatastoreToolSurfaceDocument),
        Collection::Tools => schema_for!(Tools),
        Collection::AgentTarget => schema_for!(AgentTargetDocument),
        Collection::InferenceBackend => schema_for!(InferenceBackend),
        Collection::InferenceProfile => schema_for!(InferenceProfile),
        Collection::InferenceSampling => schema_for!(InferenceSampling),
        Collection::InferenceExecution => schema_for!(InferenceExecution),
        Collection::InferenceRetryPolicy => schema_for!(InferenceRetryPolicy),
        Collection::ToolServiceRegistry => schema_for!(ToolServiceRegistry),
        Collection::ProjectionAcpBinding => schema_for!(ProjectionAcpBinding),
        Collection::Task => schema_for!(Task),
        Collection::Schedule => schema_for!(Schedule),
        Collection::EventSource => schema_for!(EventSource),
        Collection::Trigger => schema_for!(Trigger),
        Collection::Callback => schema_for!(Callback),
        Collection::CallbackBinding => schema_for!(CallbackBinding),
        Collection::CallbackModule => schema_for!(CallbackModule),
        Collection::RepositoryPlacement => schema_for!(RepositoryPlacement),
        Collection::EvalDefinition => schema_for!(EvalDefinition),
    }
}
